//! How a face reaches the screen (spec CB-24 "crisp at any zoom", §10 face-paint budget 0.5 ms).
//!
//! The painter is split in two so each half is swappable and testable on its own:
//!
//! 1. A [`FacePainter`] turns a profile and the frame's [`PaintParams`] into a [`DrawList`] per layer: resolution-independent
//!    vector commands in face-plane metres. It is plain CPU maths: deterministic, allocation-free once warm, and
//!    testable without a GPU (compare lists, or rasterise them in a test).
//! 2. A rasteriser draws a list into pixels: the GPU one (Vello or a signed-distance-field path renderer) lives in
//!    `omuzaki-face` next to the renderer glue; [`FaceRasterizer`] is the CPU one for headless tests, goldens and the
//!    WASM fallback.
//!
//! **How it meets the renderer** (`docs/design/render-api.md`, "Face painter compositing"). Each layer is one dynamic
//! texture owned by the renderer (premultiplied RGBA8, sRGB colour, mips on), and a material composites it as an
//! `Overlay` with a `Planar` mapping: [`FaceProjection::planar`] gives the exact mapping fields. `omuzaki-face` wraps a
//! `FacePainter` and a rasteriser in the renderer's `Painter` trait, so the stage calls
//! `renderer.paint_texture(layer_texture, &mut painter)` only when [`DrawList::fingerprint`] changed. A GPU
//! rasteriser draws straight into `PaintTarget::native()`; the CPU path writes [`Raster::pixels`] with `write_rgba`.
//! This crate never names a render-api type, so neither depends on the other.
//!
//! **Layers.** The `Brows` layer exists because hair composites it too, so brows read through the bangs.
//!
//! **Colour.** Colours are sRGB and the painter blends in sRGB space, like the 2D canvas the prototype used; rasterised
//! pixels are **premultiplied** alpha (clean filtering and mip-mapping on the GPU).

use std::ops::{Index, IndexMut};

use omuzaki_core::colour::{Colour, to_byte};
use omuzaki_core::glam::{Affine2, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use serde::{Deserialize, Serialize};

use crate::mapping::PaintParams;
use crate::profile::FaceProfile;

/// A painted layer of the face.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FaceLayerId {
    /// The painted face on the skin: blush, brows, eyes, nose, mouth, marks.
    Base,
    /// Only the brows. Hair composites this layer to draw brows through the bangs.
    Brows,
    /// Effects drawn in front of the hair: sweat drops, gloom lines, anger veins (reserved).
    Overlay,
}

/// Which surface a layer is composited onto.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayerTarget {
    /// The head's skin material.
    Skin,
    /// The hair material (so a layer shows through the strands).
    Hair,
    /// In front of everything, on its own transparent surface (reserved for the `Overlay` layer).
    Front,
}

/// How a layer fades in along the head's depth axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fade {
    /// The head's own [`FaceProjection::front_fade`]: a soft edge so the face never smears over the head's sides.
    Soft,
    /// A hard cut at the head centre's depth: everything in front shows, nothing behind does.
    Hard,
}

/// How the renderer composites one layer (the prototype's numbers, verified there).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Composite {
    /// The surface it goes on.
    pub target: LayerTarget,
    /// How it fades in front to back.
    pub fade: Fade,
    /// Overall opacity, `0..=1`. Brows through hair are 0.85, so the strands still read.
    pub strength: f32,
}

impl FaceLayerId {
    /// How many layers exist.
    pub const COUNT: usize = 3;
    /// Every layer, in draw order.
    pub const ALL: [FaceLayerId; Self::COUNT] = [Self::Base, Self::Brows, Self::Overlay];

    /// How the renderer composites this layer. Fixed by the face contract, not by the character, so every character
    /// composes the same way and the stage needs no per-character knowledge.
    pub fn composite(self) -> Composite {
        match self {
            Self::Base => Composite { target: LayerTarget::Skin, fade: Fade::Soft, strength: 1.0 },
            Self::Brows => Composite { target: LayerTarget::Hair, fade: Fade::Hard, strength: 0.85 },
            Self::Overlay => Composite { target: LayerTarget::Front, fade: Fade::Hard, strength: 1.0 },
        }
    }

    /// The raster density of this layer relative to the base layer: brows are thin lines on a half-size texture.
    pub fn density_ratio(self) -> f32 {
        match self {
            Self::Brows => 0.5,
            Self::Base | Self::Overlay => 1.0,
        }
    }
}

/// A rectangle in face-plane metres.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FaceRect {
    /// Left edge (toward the character's right: smaller x).
    pub x0: f32,
    /// Right edge.
    pub x1: f32,
    /// Bottom edge.
    pub y0: f32,
    /// Top edge.
    pub y1: f32,
}

impl FaceRect {
    /// Width in metres.
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    /// Height in metres.
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// The pixel size of the rect at `px_per_m`, at least 1x1.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // sizes are small positive numbers
    pub fn size_px(&self, px_per_m: f32) -> (u32, u32) {
        let px = |len: f32| (len * px_per_m).round().max(1.0) as u32;
        (px(self.width()), px(self.height()))
    }
}

/// The fields of the renderer's `Mapping::Planar`, in the mesh's local space: a copy of this is the mapping, field
/// for field. (The face contract can't name the render-api type, but the names match so the conversion is trivial.)
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PlanarWindow {
    /// Lower-left corner of the rectangle in the mesh's local XY.
    pub min: Vec2,
    /// Upper-right corner. The top row of the texture is at `max.y`.
    pub max: Vec2,
    /// Local Z where the projection starts to show.
    pub fade_from: f32,
    /// Local Z where it is fully shown. Equal to `fade_from` for a hard cut.
    pub fade_to: f32,
}

/// Where painted face layers land on the head: published by the head part, consumed by the stage and the painter.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FaceProjection {
    /// The face-plane rectangle the layers cover (head-local metres).
    pub rect: FaceRect,
    /// Head-local `z` range over which the painted face fades in toward the front ([`Fade::Soft`]): weight 0 at
    /// `front_fade[0]`, 1 at `front_fade[1]`.
    pub front_fade: [f32; 2],
}

impl FaceProjection {
    /// The planar mapping of `layer` for a mesh whose local space is bind space, given the head centre in bind space
    /// (the head-local frame's origin). This is exactly what goes into the material's `Overlay`.
    pub fn planar(&self, layer: FaceLayerId, head_centre: Vec3) -> PlanarWindow {
        let (fade_from, fade_to) = match layer.composite().fade {
            Fade::Soft => (self.front_fade[0], self.front_fade[1]),
            Fade::Hard => (0.0, 0.0),
        };
        PlanarWindow {
            min: Vec2::new(self.rect.x0 + head_centre.x, self.rect.y0 + head_centre.y),
            max: Vec2::new(self.rect.x1 + head_centre.x, self.rect.y1 + head_centre.y),
            fade_from: fade_from + head_centre.z,
            fade_to: fade_to + head_centre.z,
        }
    }
}

/// Narrows a parameter (`f64`, as documents and ops see it) to the `f32` the real-time side uses: the boundary
/// conversion every painter makes when it reads the profile. Parameters are small numbers, so nothing is lost that a
/// pixel or a vertex could show.
#[allow(clippy::cast_possible_truncation)] // see above
pub fn narrow(v: f64) -> f32 {
    v as f32
}

/// The raster density of the base layer unless a quality setting says otherwise: 3 600 px/m puts the prototype's
/// 0.43 m face window at about 1 536 px wide.
pub const DEFAULT_PX_PER_M: f32 = 3600.0;

/// What the renderer needs to know about one layer's texture.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FaceLayerSpec {
    /// Which layer.
    pub id: FaceLayerId,
    /// The face-plane rectangle it covers.
    pub rect: FaceRect,
    /// Raster density, pixels per metre. **A runtime quality knob, not part of the face profile:** the frame-budget
    /// scheduler shrinks it under load (create a smaller texture, repoint the overlay, release the old one) and the
    /// vector draw list keeps every size crisp. Use [`FaceLayerSpec::for_layer`] so layers keep their relative density.
    pub px_per_m: f32,
}

impl FaceLayerSpec {
    /// The spec of `id` over `projection` at a base density, scaled by [`FaceLayerId::density_ratio`].
    pub fn for_layer(id: FaceLayerId, projection: &FaceProjection, base_px_per_m: f32) -> Self {
        Self { id, rect: projection.rect, px_per_m: base_px_per_m * id.density_ratio() }
    }

    /// The spec of a texture that is not a face layer (a garment's plaid or trim band): `rect` at exactly `px_per_m`,
    /// with `id` as a nominal layer. Unlike [`FaceLayerSpec::for_layer`] it needs no [`FaceProjection`] and applies no
    /// density ratio, so the caller's density is the texture's density.
    pub fn for_rect(rect: FaceRect, id: FaceLayerId, px_per_m: f32) -> Self {
        Self { id, rect, px_per_m }
    }

    /// The texture size in pixels.
    pub fn size_px(&self) -> (u32, u32) {
        self.rect.size_px(self.px_per_m)
    }
}

/// Straight-alpha, sRGB-encoded components and alpha helpers the painter and rasterisers share.
pub trait ColourExt {
    /// The components as `0..=1` floats, still sRGB-encoded, with straight alpha.
    fn srgb_f32(self) -> [f32; 4];
    /// The same colour with alpha `alpha` in `0..=1` (clamped), replacing its own.
    #[must_use]
    fn faded(self, alpha: f32) -> Colour;
}

impl ColourExt for Colour {
    fn srgb_f32(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a].map(|c| f32::from(c) / 255.0)
    }

    fn faded(self, alpha: f32) -> Colour {
        Colour { a: to_byte(alpha), ..self }
    }
}

// ------------------------------------------------------------------------------------------------ draw list

/// One path element, in face-plane metres.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathEl {
    /// Starts a sub-path.
    Move(Vec2),
    /// A straight line.
    Line(Vec2),
    /// A quadratic Bézier: control point, end point.
    Quad(Vec2, Vec2),
    /// A cubic Bézier: two control points, end point.
    Cubic(Vec2, Vec2, Vec2),
    /// Closes the sub-path.
    Close,
}

/// A path stored in a [`DrawList`]: a range of its element arena.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRef {
    start: u32,
    len: u32,
}

/// One colour stop of a gradient.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Position along the gradient, 0 to 1.
    pub offset: f32,
    /// Colour at that position.
    pub colour: Colour,
}

/// Up to four gradient stops, stored inline so paints are `Copy` and commands never allocate.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stops {
    len: u8,
    items: [GradientStop; 4],
}

impl Stops {
    /// Builds a stop list from at most four `(offset, colour)` pairs; extra pairs are ignored.
    pub fn new(stops: &[(f32, Colour)]) -> Self {
        let blank = GradientStop { offset: 0.0, colour: Colour::rgb(0, 0, 0) };
        let mut items = [blank; 4];
        let mut len = 0_u8;
        for (slot, (offset, colour)) in items.iter_mut().zip(stops) {
            *slot = GradientStop { offset: *offset, colour: *colour };
            len += 1;
        }
        Self { len, items }
    }

    /// The stops in use.
    pub fn as_slice(&self) -> &[GradientStop] {
        &self.items[..usize::from(self.len)]
    }
}

/// How a fill or stroke is coloured.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Paint {
    /// One colour.
    Solid(Colour),
    /// A linear gradient between two points.
    Linear {
        /// Start point.
        from: Vec2,
        /// End point.
        to: Vec2,
        /// Colour stops.
        stops: Stops,
    },
    /// A radial gradient (stretch it with a transform for an ellipse).
    Radial {
        /// Centre.
        centre: Vec2,
        /// Radius.
        radius: f32,
        /// Colour stops.
        stops: Stops,
    },
}

/// Stroke end style.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cap {
    /// Flat at the end point.
    Butt,
    /// Half a disc beyond the end point.
    Round,
}

/// Stroke corner style.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Join {
    /// Sharp corners.
    Miter,
    /// Rounded corners.
    Round,
}

/// One drawing command. Paths fill with the non-zero rule.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Cmd {
    /// Fill a path.
    Fill {
        /// The path.
        path: PathRef,
        /// Its colouring.
        paint: Paint,
        /// Extra opacity, 0 to 1, multiplied into the paint's alpha.
        opacity: f32,
    },
    /// Stroke a path.
    Stroke {
        /// The path.
        path: PathRef,
        /// Its colouring.
        paint: Paint,
        /// Line width in metres.
        width: f32,
        /// End style.
        cap: Cap,
        /// Corner style.
        join: Join,
        /// Extra opacity, 0 to 1.
        opacity: f32,
    },
    /// Limit everything until the matching [`Cmd::PopClip`] to the inside of a path (clips nest: they intersect).
    PushClip(PathRef),
    /// Ends the innermost clip.
    PopClip,
    /// Apply an affine transform (composed with the current one) until the matching [`Cmd::PopTransform`].
    PushTransform(Affine2),
    /// Ends the innermost transform.
    PopTransform,
}

/// A vector picture: commands plus the path elements they reference. Reuse one list per layer across frames: after
/// the first frames [`DrawList::clear`] keeps the capacity, so painting allocates nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DrawList {
    cmds: Vec<Cmd>,
    els: Vec<PathEl>,
}

impl DrawList {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties the list, keeping its memory.
    pub fn clear(&mut self) {
        self.cmds.clear();
        self.els.clear();
    }

    /// Starts a path; call [`PathBuilder::finish`] to get the reference.
    pub fn path(&mut self) -> PathBuilder<'_> {
        PathBuilder { start: self.els.len(), list: self }
    }

    /// Adds a command.
    pub fn push(&mut self, cmd: Cmd) {
        self.cmds.push(cmd);
    }

    /// Fills `path` with `paint` at full opacity.
    pub fn fill(&mut self, path: PathRef, paint: Paint) {
        self.push(Cmd::Fill { path, paint, opacity: 1.0 });
    }

    /// Strokes `path` with round caps and joins.
    pub fn stroke(&mut self, path: PathRef, paint: Paint, width: f32) {
        self.push(Cmd::Stroke { path, paint, width, cap: Cap::Round, join: Join::Round, opacity: 1.0 });
    }

    /// Multiplies the opacity of every fill and stroke from command `from` on by `factor` (clamped to `0..=1`), so a
    /// group of pieces painted in sequence can be faded together without knowing what they are.
    pub fn scale_opacity_from(&mut self, from: usize, factor: f32) {
        let factor = factor.clamp(0.0, 1.0);
        for cmd in self.cmds.iter_mut().skip(from) {
            match cmd {
                Cmd::Fill { opacity, .. } | Cmd::Stroke { opacity, .. } => *opacity *= factor,
                _ => {}
            }
        }
    }

    /// The commands, in drawing order.
    pub fn cmds(&self) -> &[Cmd] {
        &self.cmds
    }

    /// The elements of a path.
    pub fn elements(&self, path: PathRef) -> &[PathEl] {
        let start = path.start as usize;
        self.els.get(start..start + path.len as usize).unwrap_or(&[])
    }

    /// True if nothing would be drawn.
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// A stable 64-bit digest of the content (FNV-1a over the exact bit patterns). Equal fingerprints mean equal
    /// pictures, so a renderer can skip re-rasterising an unchanged layer, and tests can pin a golden.
    pub fn fingerprint(&self) -> u64 {
        let mut h = Fnv::new();
        for cmd in &self.cmds {
            h.cmd(cmd, self);
        }
        h.0
    }
}

/// Builds one path inside a [`DrawList`].
pub struct PathBuilder<'a> {
    list: &'a mut DrawList,
    start: usize,
}

/// Bézier control offset that makes four cubic arcs a circle (4/3 · (√2 − 1)).
const KAPPA: f32 = 0.552_284_8;

impl PathBuilder<'_> {
    fn el(self, e: PathEl) -> Self {
        self.list.els.push(e);
        self
    }

    /// Starts a sub-path at `p`.
    #[must_use]
    pub fn move_to(self, p: Vec2) -> Self {
        self.el(PathEl::Move(p))
    }

    /// A line to `p`.
    #[must_use]
    pub fn line_to(self, p: Vec2) -> Self {
        self.el(PathEl::Line(p))
    }

    /// A quadratic curve to `end` via `ctrl`.
    #[must_use]
    pub fn quad_to(self, ctrl: Vec2, end: Vec2) -> Self {
        self.el(PathEl::Quad(ctrl, end))
    }

    /// A cubic curve to `end` via `c1` and `c2`.
    #[must_use]
    pub fn cubic_to(self, c1: Vec2, c2: Vec2, end: Vec2) -> Self {
        self.el(PathEl::Cubic(c1, c2, end))
    }

    /// Closes the sub-path.
    #[must_use]
    pub fn close(self) -> Self {
        self.el(PathEl::Close)
    }

    /// A line through `points` (a move to the first, lines to the rest). Does nothing for an empty slice.
    #[must_use]
    pub fn polyline(mut self, points: &[Vec2]) -> Self {
        for (i, p) in points.iter().enumerate() {
            self = if i == 0 { self.move_to(*p) } else { self.line_to(*p) };
        }
        self
    }

    /// A closed ellipse centred at `c` with radii `rx` and `ry`.
    #[must_use]
    pub fn ellipse(self, c: Vec2, rx: f32, ry: f32) -> Self {
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        self.move_to(c + Vec2::new(rx, 0.0))
            .cubic_to(c + Vec2::new(rx, ky), c + Vec2::new(kx, ry), c + Vec2::new(0.0, ry))
            .cubic_to(c + Vec2::new(-kx, ry), c + Vec2::new(-rx, ky), c + Vec2::new(-rx, 0.0))
            .cubic_to(c + Vec2::new(-rx, -ky), c + Vec2::new(-kx, -ry), c + Vec2::new(0.0, -ry))
            .cubic_to(c + Vec2::new(kx, -ry), c + Vec2::new(rx, -ky), c + Vec2::new(rx, 0.0))
            .close()
    }

    /// Ends the path and returns its reference.
    #[allow(clippy::cast_possible_truncation)] // a face list never holds 4 billion path elements
    pub fn finish(self) -> PathRef {
        PathRef { start: self.start as u32, len: (self.list.els.len() - self.start) as u32 }
    }
}

/// FNV-1a over a command stream; hand-rolled so the digest is identical on every platform and Rust version.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }

    fn u32(&mut self, v: u32) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }

    fn f(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    fn v2(&mut self, v: Vec2) {
        self.f(v.x);
        self.f(v.y);
    }

    fn colour(&mut self, c: Colour) {
        for b in [c.r, c.g, c.b, c.a] {
            self.byte(b);
        }
    }

    fn paint(&mut self, p: &Paint) {
        let stops = |h: &mut Self, s: &Stops| {
            for st in s.as_slice() {
                h.f(st.offset);
                h.colour(st.colour);
            }
        };
        match p {
            Paint::Solid(c) => {
                self.byte(1);
                self.colour(*c);
            }
            Paint::Linear { from, to, stops: s } => {
                self.byte(2);
                self.v2(*from);
                self.v2(*to);
                stops(self, s);
            }
            Paint::Radial { centre, radius, stops: s } => {
                self.byte(3);
                self.v2(*centre);
                self.f(*radius);
                stops(self, s);
            }
        }
    }

    fn path(&mut self, list: &DrawList, p: PathRef) {
        for e in list.elements(p) {
            match e {
                PathEl::Move(a) => {
                    self.byte(10);
                    self.v2(*a);
                }
                PathEl::Line(a) => {
                    self.byte(11);
                    self.v2(*a);
                }
                PathEl::Quad(a, b) => {
                    self.byte(12);
                    self.v2(*a);
                    self.v2(*b);
                }
                PathEl::Cubic(a, b, c) => {
                    self.byte(13);
                    self.v2(*a);
                    self.v2(*b);
                    self.v2(*c);
                }
                PathEl::Close => self.byte(14),
            }
        }
        self.byte(15);
    }

    fn cmd(&mut self, cmd: &Cmd, list: &DrawList) {
        match cmd {
            Cmd::Fill { path, paint, opacity } => {
                self.byte(20);
                self.path(list, *path);
                self.paint(paint);
                self.f(*opacity);
            }
            Cmd::Stroke { path, paint, width, cap, join, opacity } => {
                self.byte(21);
                self.path(list, *path);
                self.paint(paint);
                self.f(*width);
                self.byte(u8::from(*cap == Cap::Round));
                self.byte(u8::from(*join == Join::Round));
                self.f(*opacity);
            }
            Cmd::PushClip(p) => {
                self.byte(22);
                self.path(list, *p);
            }
            Cmd::PopClip => self.byte(23),
            Cmd::PushTransform(t) => {
                self.byte(24);
                for v in t.to_cols_array() {
                    self.f(v);
                }
            }
            Cmd::PopTransform => self.byte(25),
        }
    }
}

/// One [`DrawList`] per layer: the painter's output, owned by the caller and reused every frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FaceDrawing {
    lists: [DrawList; FaceLayerId::COUNT],
}

impl FaceDrawing {
    /// Empty lists for every layer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Empties every list, keeping memory.
    pub fn clear(&mut self) {
        for list in &mut self.lists {
            list.clear();
        }
    }

    /// The fingerprint of every layer, in [`FaceLayerId::ALL`] order. The stage keeps the last ones and repaints a
    /// layer's texture only where they differ (an unchanged face costs nothing).
    pub fn fingerprints(&self) -> [u64; FaceLayerId::COUNT] {
        [self.lists[0].fingerprint(), self.lists[1].fingerprint(), self.lists[2].fingerprint()]
    }
}

impl Index<FaceLayerId> for FaceDrawing {
    type Output = DrawList;
    fn index(&self, id: FaceLayerId) -> &DrawList {
        &self.lists[id as usize]
    }
}

impl IndexMut<FaceLayerId> for FaceDrawing {
    fn index_mut(&mut self, id: FaceLayerId) -> &mut DrawList {
        &mut self.lists[id as usize]
    }
}

// ------------------------------------------------------------------------------------------------ painter and rasteriser

/// Per-frame inputs that are not channels.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FaceView {
    /// How far reflections slide, in `-1..=1` per axis: opposite the direction the camera sees the face from
    /// (the character's left is +x). The caller computes it from the camera and head; zero when unknown.
    pub highlight_shift: Vec2,
}

/// Everything a painter reads for one frame.
#[derive(Copy, Clone, Debug)]
pub struct FaceInput<'a> {
    /// The character's face design, with palette slots already resolved: a painter never sees an `@slot`. The stage
    /// calls [`FaceProfile::resolve_colours`] before painting, and again whenever the palette changes.
    pub profile: &'a FaceProfile,
    /// What to paint, mapped from the bus with `profile.mapping` (`FaceMapping::apply`). Mapped once by the caller, so
    /// several painters (the live face, a bake, a thumbnail) agree.
    pub params: &'a PaintParams,
    /// Seconds on the shot's clock, for twinkle and shimmer (never the wall clock: renders must replay exactly).
    pub time: Seconds,
    /// View-dependent inputs.
    pub view: FaceView,
}

/// A painter or rasteriser could not do its job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FaceError {
    /// The profile or frame can't be painted (non-finite values, an unsupported feature).
    #[error("can't paint this face: {0}")]
    Invalid(String),
    /// The rasteriser can't handle the list or target.
    #[error("can't rasterise: {0}")]
    Raster(String),
}

/// Turns a face profile and channel values into draw lists. Real-time: `paint` must not allocate once the lists have
/// warmed up, must not lock, and must be a pure function of its input (same input, same lists, on every platform).
pub trait FacePainter: Send {
    /// The layers this painter fills and where they sit.
    fn layers(&self) -> &[FaceLayerSpec];

    /// Clears and refills `out` for this frame.
    ///
    /// # Errors
    /// [`FaceError::Invalid`] if the profile or frame can't be painted; `out` is then unspecified.
    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError>;
}

/// A premultiplied-alpha RGBA8 image, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes: R, G, B, A, premultiplied, sRGB-encoded colour.
    pub pixels: Vec<u8>,
}

impl Raster {
    /// A transparent image.
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, pixels: vec![0; width as usize * height as usize * 4] }
    }
}

/// Draws a [`DrawList`] into pixels on the CPU. Row 0 of the image is the top of `spec.rect`.
pub trait FaceRasterizer: Send {
    /// Rasterises `list` over `spec.rect` into `out`, resizing `out` to `spec.rect` at `px_per_m`.
    ///
    /// # Errors
    /// [`FaceError::Raster`] if the list uses something this rasteriser can't draw.
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint_sample(l: &mut DrawList, rx: f32) {
        let p = l.path().ellipse(Vec2::new(0.01, -0.02), rx, 0.01).finish();
        l.fill(p, Paint::Solid(Colour::rgb(255, 0, 0)));
        let q = l.path().polyline(&[Vec2::ZERO, Vec2::new(0.1, 0.0)]).finish();
        l.stroke(q, Paint::Solid(Colour::rgb(0, 0, 0)), 0.002);
    }

    #[test]
    fn scale_opacity_from_scales_fills_and_strokes_from_the_mark_on() {
        let mut l = DrawList::new();
        paint_sample(&mut l, 0.02);
        let mark = l.cmds().len();
        paint_sample(&mut l, 0.03);
        l.push(Cmd::PopClip);
        l.scale_opacity_from(mark, 0.5);
        let opacities: Vec<Option<f32>> = l
            .cmds()
            .iter()
            .map(|c| match c {
                Cmd::Fill { opacity, .. } | Cmd::Stroke { opacity, .. } => Some(*opacity),
                _ => None,
            })
            .collect();
        assert_eq!(opacities, [Some(1.0), Some(1.0), Some(0.5), Some(0.5), None], "only the commands from the mark");
        // A second scaling multiplies, a factor over 1 is clamped, and a mark past the end touches nothing.
        l.scale_opacity_from(mark, 0.5);
        l.scale_opacity_from(mark, 4.0);
        l.scale_opacity_from(99, 0.0);
        assert!(matches!(l.cmds()[2], Cmd::Fill { opacity, .. } if (opacity - 0.25).abs() < 1e-6));
        assert!(matches!(l.cmds()[0], Cmd::Fill { opacity, .. } if (opacity - 1.0).abs() < 1e-6));
    }

    fn sample(rx: f32) -> DrawList {
        let mut l = DrawList::new();
        paint_sample(&mut l, rx);
        l
    }

    #[test]
    fn paths_are_stored_in_an_arena_and_read_back() {
        let l = sample(0.02);
        let Cmd::Fill { path, .. } = l.cmds()[0] else { panic!("first command is the fill") };
        let els = l.elements(path);
        assert_eq!((els.len(), els[0], els[5]), (6, PathEl::Move(Vec2::new(0.03, -0.02)), PathEl::Close));
        let Cmd::Stroke { path, .. } = l.cmds()[1] else { panic!("second command is the stroke") };
        assert_eq!(l.elements(path).len(), 2);
    }

    #[test]
    fn fingerprints_are_stable_and_content_sensitive() {
        assert_eq!(sample(0.02).fingerprint(), sample(0.02).fingerprint());
        assert_ne!(sample(0.02).fingerprint(), sample(0.021).fingerprint());
        assert_ne!(DrawList::new().fingerprint(), sample(0.02).fingerprint());
        // Pinned: if this changes, every golden list hash changes with it.
        assert_eq!(DrawList::new().fingerprint(), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn clearing_keeps_memory_so_repainting_does_not_allocate() {
        let mut l = sample(0.02);
        l.clear();
        assert!(l.is_empty());
        let (c, e) = (l.cmds.capacity(), l.els.capacity());
        assert!(c > 0 && e > 0, "clear keeps the arenas");
        paint_sample(&mut l, 0.03);
        assert_eq!((l.cmds.capacity(), l.els.capacity()), (c, e), "an equal-sized repaint reuses the memory");
    }

    #[test]
    fn stops_hold_four_inline_and_ignore_the_rest() {
        let c = Colour::rgb(1, 2, 3);
        let s = Stops::new(&[(0.0, c), (0.3, c), (0.6, c), (0.9, c), (1.0, c)]);
        assert_eq!(s.as_slice().len(), 4);
        assert_eq!(Stops::new(&[]).as_slice().len(), 0);
    }

    #[test]
    fn rects_convert_to_pixel_sizes() {
        let r = FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 0.0 };
        assert_eq!(r.size_px(1000.0), (400, 300));
        assert_eq!(r.size_px(0.0), (1, 1));
    }

    #[test]
    fn layers_composite_the_way_the_prototype_does() {
        let base = FaceLayerId::Base.composite();
        assert_eq!((base.target, base.fade, base.strength), (LayerTarget::Skin, Fade::Soft, 1.0));
        let brows = FaceLayerId::Brows.composite();
        assert_eq!((brows.target, brows.fade, brows.strength), (LayerTarget::Hair, Fade::Hard, 0.85));
        assert_eq!(FaceLayerId::Brows.density_ratio(), 0.5);
    }

    #[test]
    fn planar_windows_move_into_the_meshs_space() {
        let proj = FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 0.1 }, front_fade: [0.0, 0.07] };
        let centre = Vec3::new(0.0, 0.8, -0.02);
        let skin = proj.planar(FaceLayerId::Base, centre);
        assert!(skin.min.abs_diff_eq(Vec2::new(-0.2, 0.5), 1e-6) && skin.max.abs_diff_eq(Vec2::new(0.2, 0.9), 1e-6));
        assert!((skin.fade_from - -0.02).abs() < 1e-6 && (skin.fade_to - 0.05).abs() < 1e-6, "soft: the head's fade");
        let brows = proj.planar(FaceLayerId::Brows, centre);
        assert_eq!(brows.fade_from, brows.fade_to, "brows cut hard");
        assert_eq!(brows.min, skin.min, "the same rectangle, so brows line up exactly");
    }

    #[test]
    fn layer_specs_scale_with_the_quality_knob() {
        let proj =
            FaceProjection { rect: FaceRect { x0: -0.215, x1: 0.215, y0: -0.2, y1: 0.1 }, front_fade: [0.0, 0.07] };
        let base = FaceLayerSpec::for_layer(FaceLayerId::Base, &proj, DEFAULT_PX_PER_M);
        let brows = FaceLayerSpec::for_layer(FaceLayerId::Brows, &proj, DEFAULT_PX_PER_M);
        assert_eq!(base.size_px().0, 1548, "about the prototype's 1536 px face atlas");
        assert_eq!(brows.size_px().0, 774, "and its 768 px brow layer");
        assert_eq!(
            FaceLayerSpec::for_layer(FaceLayerId::Base, &proj, DEFAULT_PX_PER_M / 2.0).size_px().0,
            774,
            "the scheduler halves it"
        );
    }

    #[test]
    fn a_rect_spec_is_a_texture_at_the_callers_density() {
        let rect = FaceRect { x0: 0.0, x1: 0.25, y0: 0.0, y1: 0.125 };
        let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Brows, 512.0);
        assert_eq!((spec.id, spec.rect, spec.px_per_m), (FaceLayerId::Brows, rect, 512.0));
        assert_eq!(spec.size_px(), (128, 64), "no density ratio: Brows is not halved here");
    }

    #[test]
    fn colour_helpers_keep_srgb_and_replace_alpha() {
        let c = Colour::rgb(255, 0, 51).faded(0.5);
        assert_eq!(c.a, 128);
        assert_eq!(c.srgb_f32(), [1.0, 0.0, 0.2, 128.0 / 255.0]);
    }

    #[test]
    fn drawings_index_by_layer() {
        let mut d = FaceDrawing::new();
        d[FaceLayerId::Brows] = sample(0.02);
        assert!(d[FaceLayerId::Base].is_empty() && !d[FaceLayerId::Brows].is_empty());
        let fp = d.fingerprints();
        assert_eq!(fp[0], DrawList::new().fingerprint());
        assert_eq!(fp[1], sample(0.02).fingerprint());
        d.clear();
        assert!(d[FaceLayerId::Brows].is_empty());
    }
}
