//! Particles (ST-13, `docs/design/particles.md`): confetti, emote rain, rain, snow, petals and hit stars.
//!
//! **Stateless by design (D1).** A particle is a pure function of the batch it was born in, its index and the scene
//! time: nothing about it is stored, on the CPU or the GPU. A [`ParticleSystemDesc`] (how particles look and move) is
//! a resource, created once and referenced by [`ParticleSystemId`]; a [`SpawnBatch`] (particles born together) is a
//! value the stage puts in [`Scene::particles`](crate::Scene::particles) every frame. Any frame can therefore be drawn
//! on its own, offline renders never depend on earlier frames, and a renderer rebuilt after a device loss draws every
//! particle where it was.
//!
//! **One reference definition (D3).** [`evaluate`] is the motion, life and randomness of every particle. The mock
//! draws with it, and the GPU shader is a line-for-line port tested against it, so the two agree within rounding.
//! [`particle_random`] and the [`Stream`] numbers are part of the contract: changing them changes every golden.
//!
//! Parameter types ([`ParticleParams`], [`SpriteShape`], [`Span`], [`ColourSet`], [`Orientation`],
//! [`ParticleShading`], [`Edge`], [`Flutter`]) serialise with JSON Schemas, so presets and the scene's weather store
//! them; run-time types that hold handles ([`ParticleSystemDesc`], [`Sprite`], [`SpawnBatch`]) don't.

use std::f32::consts::TAU;

use omuzaki_core::glam::{Mat3, Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::{MAX_ATLAS_CELLS, MAX_PARTICLE_LIFETIME_S, MAX_PARTICLES, MAX_SPAWN_BATCHES};
use crate::{Colour, ParticleSystemId, RenderError, TextureId};

/// How a system's particles look and move. Created once with
/// [`Renderer::create_particle_system`](crate::Renderer::create_particle_system), referenced by handle.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticleSystemDesc {
    /// What each particle looks like.
    pub sprite: Sprite,
    /// How it moves, lives and is shaded.
    pub params: ParticleParams,
}

impl ParticleSystemDesc {
    /// Checks every rule of D2 that doesn't need the renderer's tables (a dead atlas texture is the renderer's to
    /// find).
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the first broken rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        match self.sprite {
            Sprite::Shape(shape) => shape.validate()?,
            Sprite::Atlas { columns, rows, .. } => {
                let cells = u32::from(columns) * u32::from(rows);
                if cells == 0 || cells > MAX_ATLAS_CELLS {
                    return Err(RenderError::invalid(
                        "particle system",
                        format!("an atlas needs 1..={MAX_ATLAS_CELLS} cells, got {columns} x {rows}"),
                    ));
                }
                if self.params.edge.is_some() {
                    return Err(RenderError::invalid("particle system", "an edge is for shapes only, not an atlas"));
                }
            }
        }
        if matches!(self.sprite, Sprite::Shape(SpriteShape::Streak))
            && !matches!(self.params.orientation, Orientation::Velocity { .. })
        {
            return Err(RenderError::invalid("particle system", "a streak needs `orientation = velocity`"));
        }
        self.params.validate()
    }

    /// The number of atlas cells, or 0 for a shape.
    pub fn cells(&self) -> u32 {
        match self.sprite {
            Sprite::Shape(_) => 0,
            Sprite::Atlas { columns, rows, .. } => u32::from(columns) * u32::from(rows),
        }
    }

    /// The width over the height of a particle's quad: a rectangle's `aspect`, otherwise 1.
    pub fn aspect(&self) -> f32 {
        match self.sprite {
            Sprite::Shape(SpriteShape::Rect { aspect }) => aspect,
            _ => 1.0,
        }
    }
}

/// What a particle looks like.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Sprite {
    /// Drawn by the shader from a signed distance: crisp at any size, no texture, no asset.
    Shape(SpriteShape),
    /// Cells of a texture atlas, row-major from the top left (chat emotes, a sprite sheet). The texture is the
    /// caller's, premultiplied; releasing it while a system reads it is `InUse`.
    #[non_exhaustive]
    Atlas {
        /// The atlas texture.
        texture: TextureId,
        /// Cells across.
        columns: u16,
        /// Cells down.
        rows: u16,
    },
}

impl Sprite {
    /// An atlas sprite of `columns` x `rows` cells.
    pub const fn atlas(texture: TextureId, columns: u16, rows: u16) -> Self {
        Self::Atlas { texture, columns, rows }
    }

    /// The atlas texture, if any.
    pub const fn texture(&self) -> Option<TextureId> {
        match self {
            Self::Shape(_) => None,
            Self::Atlas { texture, .. } => Some(*texture),
        }
    }
}

/// A procedural sprite shape.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum SpriteShape {
    /// A square.
    Square,
    /// A rectangle, `aspect` (width / height) in `0.1..=10`: a confetti paper strip.
    #[non_exhaustive]
    Rect {
        /// Width over height, `0.1..=10`.
        aspect: f32,
    },
    /// A round disc: snow, bubbles.
    Disc,
    /// A pointed ellipse: a petal.
    Petal,
    /// A star with `points` points, `4..=8`.
    #[non_exhaustive]
    Star {
        /// Points, `4..=8`.
        points: u8,
    },
    /// A heart.
    Heart,
    /// A capsule along the velocity: rain. Needs [`Orientation::Velocity`].
    Streak,
}

impl SpriteShape {
    /// A rectangle `aspect` wide per unit of height.
    pub const fn rect(aspect: f32) -> Self {
        Self::Rect { aspect }
    }

    /// A star with `points` points.
    pub const fn star(points: u8) -> Self {
        Self::Star { points }
    }

    /// A stable number per shape, for shaders and pipeline keys.
    pub const fn code(self) -> u32 {
        match self {
            Self::Square => 0,
            Self::Rect { .. } => 1,
            Self::Disc => 2,
            Self::Petal => 3,
            Self::Star { .. } => 4,
            Self::Heart => 5,
            Self::Streak => 6,
        }
    }

    fn validate(self) -> Result<(), RenderError> {
        match self {
            Self::Rect { aspect } if !(0.1..=10.0).contains(&aspect) => Err(RenderError::invalid(
                "particle system",
                format!("a rect's aspect must be in 0.1..=10, got {aspect}"),
            )),
            Self::Star { points } if !(4..=8).contains(&points) => {
                Err(RenderError::invalid("particle system", format!("a star needs 4..=8 points, got {points}")))
            }
            _ => Ok(()),
        }
    }
}

/// A range a particle draws one value from, uniformly. Written `[min, max]`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "[f32; 2]", into = "[f32; 2]")]
pub struct Span {
    /// The smallest value.
    pub min: f32,
    /// The largest value, `>= min`.
    pub max: f32,
}

impl Span {
    /// A span from `min` to `max`.
    pub const fn new(min: f32, max: f32) -> Self {
        Self { min, max }
    }

    /// A span of one value.
    pub const fn fixed(value: f32) -> Self {
        Self { min: value, max: value }
    }

    /// The value at `u` in `[0, 1)`: `min + (max - min) u`.
    pub fn at(self, u: f32) -> f32 {
        self.min + (self.max - self.min) * u
    }

    fn check(self, what: &str, lo: f32, hi: f32) -> Result<(), RenderError> {
        if !(self.min.is_finite() && self.max.is_finite()) || self.min > self.max || self.min < lo || self.max > hi {
            return Err(RenderError::invalid(
                "particle system",
                format!("{what} must be finite with {lo} <= min <= max <= {hi}, got [{}, {}]", self.min, self.max),
            ));
        }
        Ok(())
    }
}

impl From<[f32; 2]> for Span {
    fn from([min, max]: [f32; 2]) -> Self {
        Self { min, max }
    }
}

impl From<Span> for [f32; 2] {
    fn from(s: Span) -> Self {
        [s.min, s.max]
    }
}

/// One to eight colours; each particle takes one. Written as a list. `Copy`, so a [`SpawnBatch`] can carry one.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "Vec<Colour>", into = "Vec<Colour>")]
pub struct ColourSet {
    colours: [Colour; ColourSet::MAX],
    len: u8,
}

impl ColourSet {
    /// The most colours in a set.
    pub const MAX: usize = 8;

    /// A set of `colours`, or `None` for none or more than [`ColourSet::MAX`].
    pub fn new(colours: &[Colour]) -> Option<Self> {
        if colours.is_empty() || colours.len() > Self::MAX {
            return None;
        }
        let mut set = Self { colours: [Colour::WHITE; Self::MAX], len: 0 };
        set.colours[..colours.len()].copy_from_slice(colours);
        set.len = u8::try_from(colours.len()).ok()?;
        Some(set)
    }

    /// One colour.
    pub const fn one(colour: Colour) -> Self {
        Self { colours: [colour; Self::MAX], len: 1 }
    }

    /// The colours.
    pub fn as_slice(&self) -> &[Colour] {
        &self.colours[..usize::from(self.len)]
    }

    /// How many colours (at least 1).
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Never true for a set built through [`ColourSet::new`]; here for clippy's sake.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The colour at `u` in `[0, 1)`: `colours[min(floor(u n), n - 1)]`.
    pub fn pick(&self, u: f32) -> Colour {
        let n = self.len.max(1);
        let i = pick_index(u, u32::from(n));
        self.colours[i as usize]
    }
}

impl TryFrom<Vec<Colour>> for ColourSet {
    type Error = String;

    fn try_from(colours: Vec<Colour>) -> Result<Self, Self::Error> {
        Self::new(&colours).ok_or_else(|| format!("a colour set holds 1 to 8 colours, got {}", colours.len()))
    }
}

impl From<ColourSet> for Vec<Colour> {
    fn from(set: ColourSet) -> Self {
        set.as_slice().to_vec()
    }
}

/// A horizontal sway: leaves, snow.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Flutter {
    /// How far it sways, in metres, `0..=1`.
    pub amplitude_m: f32,
    /// How often, in hertz, `0..=10`.
    pub frequency_hz: f32,
}

/// How a particle's quad is turned.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Orientation {
    /// Faces the camera, rolled by a random angle plus a spin.
    #[non_exhaustive]
    Billboard {
        /// The spin in degrees per second, drawn per particle (negative turns the other way).
        #[serde(default = "zero_span")]
        spin_deg: Span,
    },
    /// The long side follows the velocity on screen: `size + stretch × |v|` long and `size` wide (rain).
    #[non_exhaustive]
    Velocity {
        /// Extra length per metre per second of speed, `>= 0`.
        stretch: f32,
    },
    /// A world-space quad that flips like paper about a random axis (confetti, petals).
    #[non_exhaustive]
    Tumble {
        /// The turn rate in degrees per second, drawn per particle.
        rate_deg: Span,
    },
}

const fn zero_span() -> Span {
    Span::fixed(0.0)
}

impl Orientation {
    /// Facing the camera, spinning at `spin_deg` degrees per second.
    pub const fn billboard(spin_deg: Span) -> Self {
        Self::Billboard { spin_deg }
    }

    /// Along the velocity, stretched by `stretch` per metre per second.
    pub const fn velocity(stretch: f32) -> Self {
        Self::Velocity { stretch }
    }

    /// Tumbling at `rate_deg` degrees per second.
    pub const fn tumble(rate_deg: Span) -> Self {
        Self::Tumble { rate_deg }
    }

    /// A stable number per orientation, for shaders and pipeline keys.
    pub const fn code(self) -> u32 {
        match self {
            Self::Billboard { .. } => 0,
            Self::Velocity { .. } => 1,
            Self::Tumble { .. } => 2,
        }
    }
}

/// How a particle is lit (D5).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParticleShading {
    /// The colour as it is: emotes, rain, sparkles.
    Unlit,
    /// The surfaces' toon model at the particle's centre, so it has the character's terminator and tinted shade.
    #[non_exhaustive]
    Toon {
        /// The colour multiplied in on the shaded side (never grey, LK-01).
        shade: Colour,
        /// Moves the terminator, `-1..=1`.
        shift: f32,
        /// Width of the light-to-shade step, `0..=1`.
        softness: f32,
    },
}

impl ParticleShading {
    /// Toon shading with this shade colour, shift and softness.
    pub const fn toon(shade: Colour, shift: f32, softness: f32) -> Self {
        Self::Toon { shade, shift, softness }
    }
}

/// A line inside a shape's rim (shapes only; LK-02's darker shade of the particle's own colour).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    /// The line's colour as a share of the particle's colour, `0..=1`.
    pub shade: f32,
    /// Its width in pixels, `0.5..=4`.
    pub width_px: f32,
}

/// How a system's particles move, live and look (D2, D3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParticleParams {
    /// Seconds; `0 < min <= max <=` [`MAX_PARTICLE_LIFETIME_S`].
    pub lifetime: Span,
    /// Metres per second along the emission direction, `>= 0`.
    pub speed: Span,
    /// The emission axis in emitter space; not zero.
    pub direction: [f32; 3],
    /// The cone's half-angle in degrees, `0..=180` (180: every direction).
    #[serde(default)]
    pub spread_deg: f32,
    /// Metres per second squared, world space.
    pub gravity: [f32; 3],
    /// Per second, `>= 0`: the velocity relaxes toward `wind` at this rate.
    #[serde(default)]
    pub drag: f32,
    /// Metres per second, world space; only felt through drag.
    #[serde(default)]
    pub wind: [f32; 3],
    /// A horizontal sway.
    #[serde(default)]
    pub flutter: Option<Flutter>,
    /// Metres: a sprite's height (a streak's width), `> 0`.
    pub size: Span,
    /// The size multiplier at life 0, 1/3, 2/3 and 1, piecewise linear, each `>= 0`. Particles pop in and shrink away
    /// with it, because a coverage particle can't fade (D4).
    #[serde(default = "flat_curve")]
    pub size_curve: [f32; 4],
    /// How the quad is turned.
    pub orientation: Orientation,
    /// One colour per particle, drawn from the set. For an atlas, a multiplier (white: as is).
    pub colours: ColourSet,
    /// `0..=1`: the share of a pixel's samples a particle covers (D4); rain uses about 0.55.
    #[serde(default = "one")]
    pub coverage: f32,
    /// How it is lit.
    pub shading: ParticleShading,
    /// A line inside a shape's rim.
    #[serde(default)]
    pub edge: Option<Edge>,
    /// `>= 0`: the colour is multiplied by `1 + glow`, so bloom picks it up.
    #[serde(default)]
    pub glow: f32,
    /// The world height below which a particle is not drawn (rain stops at the floor).
    #[serde(default)]
    pub floor: Option<f32>,
}

const fn flat_curve() -> [f32; 4] {
    [1.0; 4]
}

const fn one() -> f32 {
    1.0
}

impl ParticleParams {
    /// Checks the ranges of D2.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the first broken rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: String| Err(RenderError::invalid("particle system", reason));
        let finite3 = |v: [f32; 3]| v.iter().all(|x| x.is_finite());
        self.lifetime.check("lifetime", 0.0, MAX_PARTICLE_LIFETIME_S)?;
        if self.lifetime.min <= 0.0 {
            return bad(format!("lifetime must be above 0, got {}", self.lifetime.min));
        }
        self.speed.check("speed", 0.0, f32::MAX)?;
        self.size.check("size", 0.0, f32::MAX)?;
        if self.size.min <= 0.0 {
            return bad(format!("size must be above 0, got {}", self.size.min));
        }
        if !finite3(self.direction) || Vec3::from(self.direction).length_squared() < 1e-12 {
            return bad(format!("direction must be a finite, non-zero vector, got {:?}", self.direction));
        }
        if !(0.0..=180.0).contains(&self.spread_deg) {
            return bad(format!("spread_deg must be in 0..=180, got {}", self.spread_deg));
        }
        if !finite3(self.gravity) || !finite3(self.wind) {
            return bad("gravity and wind must be finite".into());
        }
        if !(self.drag >= 0.0 && self.drag.is_finite()) {
            return bad(format!("drag must be a finite number >= 0, got {}", self.drag));
        }
        if let Some(f) = self.flutter
            && !((0.0..=1.0).contains(&f.amplitude_m) && (0.0..=10.0).contains(&f.frequency_hz))
        {
            return bad(format!("flutter needs amplitude_m in 0..=1 and frequency_hz in 0..=10, got {f:?}"));
        }
        if !self.size_curve.iter().all(|c| *c >= 0.0 && c.is_finite()) {
            return bad(format!("size_curve values must be finite and >= 0, got {:?}", self.size_curve));
        }
        match self.orientation {
            Orientation::Billboard { spin_deg } => spin_deg.check("spin_deg", f32::MIN, f32::MAX)?,
            Orientation::Velocity { stretch } if !(stretch >= 0.0 && stretch.is_finite()) => {
                return bad(format!("stretch must be a finite number >= 0, got {stretch}"));
            }
            Orientation::Velocity { .. } => {}
            Orientation::Tumble { rate_deg } => rate_deg.check("rate_deg", f32::MIN, f32::MAX)?,
        }
        if self.colours.is_empty() {
            return bad("colours needs 1 to 8 colours".into());
        }
        if !(0.0..=1.0).contains(&self.coverage) {
            return bad(format!("coverage must be in 0..=1, got {}", self.coverage));
        }
        if let ParticleShading::Toon { shift, softness, .. } = self.shading
            && !((-1.0..=1.0).contains(&shift) && (0.0..=1.0).contains(&softness))
        {
            return bad(format!("toon needs shift in -1..=1 and softness in 0..=1, got {shift} and {softness}"));
        }
        if let Some(e) = self.edge
            && !((0.0..=1.0).contains(&e.shade) && (0.5..=4.0).contains(&e.width_px))
        {
            return bad(format!("edge needs shade in 0..=1 and width_px in 0.5..=4, got {e:?}"));
        }
        if !(self.glow >= 0.0 && self.glow.is_finite()) {
            return bad(format!("glow must be a finite number >= 0, got {}", self.glow));
        }
        if self.floor.is_some_and(|f| !f.is_finite()) {
            return bad("floor must be finite".into());
        }
        Ok(())
    }
}

/// Where in the emitter particles are born (emitter space).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EmitterShape {
    /// The emitter's origin.
    Point,
    /// Uniformly inside a ball.
    #[non_exhaustive]
    Sphere {
        /// Metres, `>= 0`.
        radius: f32,
    },
    /// Uniformly inside a box.
    #[non_exhaustive]
    Box {
        /// Half extents in metres, each `>= 0`.
        half: Vec3,
    },
    /// Uniformly on a rectangle in the emitter's XZ plane (a weather ceiling).
    #[non_exhaustive]
    Rect {
        /// Half extents along X and Z in metres, each `>= 0`.
        half: Vec2,
    },
}

impl EmitterShape {
    /// A ball of `radius` metres.
    pub const fn sphere(radius: f32) -> Self {
        Self::Sphere { radius }
    }

    /// A box with half extents `half`.
    pub const fn cuboid(half: Vec3) -> Self {
        Self::Box { half }
    }

    /// A rectangle in the XZ plane with half extents `half` (X, Z).
    pub const fn rect(half: Vec2) -> Self {
        Self::Rect { half }
    }

    /// A stable number per shape, for shaders.
    pub const fn code(self) -> u32 {
        match self {
            Self::Point => 0,
            Self::Sphere { .. } => 1,
            Self::Box { .. } => 2,
            Self::Rect { .. } => 3,
        }
    }

    /// The shape's sizes as three numbers: the radius, or the half extents (a rectangle's are X, 0, Z).
    pub fn extent(self) -> Vec3 {
        match self {
            Self::Point => Vec3::ZERO,
            Self::Sphere { radius } => Vec3::splat(radius),
            Self::Box { half } => half,
            Self::Rect { half } => Vec3::new(half.x, 0.0, half.y),
        }
    }
}

/// Which atlas cells a batch's particles show (ignored for shapes).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CellPick {
    /// Any cell of the atlas.
    All,
    /// One of up to eight listed cells (one chat message's emotes).
    #[non_exhaustive]
    List {
        /// The cells; only the first `len` count.
        cells: [u16; 8],
        /// How many, `1..=8`.
        len: u8,
    },
}

impl CellPick {
    /// A pick from `cells`, or `None` for none or more than eight.
    pub fn list(cells: &[u16]) -> Option<Self> {
        if cells.is_empty() || cells.len() > 8 {
            return None;
        }
        let mut out = [0; 8];
        out[..cells.len()].copy_from_slice(cells);
        Some(Self::List { cells: out, len: u8::try_from(cells.len()).ok()? })
    }

    /// The listed cells, empty for [`CellPick::All`].
    pub fn as_slice(&self) -> &[u16] {
        match self {
            Self::All => &[],
            Self::List { cells, len } => &cells[..usize::from(*len).min(8)],
        }
    }
}

/// Particles born together: a burst, or one segment of a steady stream (D2). Particle `g` (from `first` to
/// `first + count`) is born at `start + (g - first) × interval`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SpawnBatch {
    /// The system that says how they look and move.
    pub system: ParticleSystemId,
    /// Emitter to world, fixed for the batch.
    pub emitter: Mat4,
    /// Where in the emitter they are born.
    pub shape: EmitterShape,
    /// Metres per second added to every particle: the anchor's own motion.
    pub velocity: Vec3,
    /// The scene time at which particle `first` was born.
    pub start: Seconds,
    /// Seconds between births, `>= 0`; 0 is a burst (all at `start`).
    pub interval: f32,
    /// The index of the batch's first particle; earlier ones have been dropped.
    pub first: u32,
    /// Particles `first .. first + count`.
    pub count: u32,
    /// The seed of every random choice.
    pub seed: u32,
    /// Atlas cells; ignored for shapes.
    pub cells: CellPick,
    /// Replaces the system's colours (a burst in the subject's palette).
    pub colours: Option<ColourSet>,
    /// A size multiplier, `> 0`; a clear shrinks it toward nothing (D12).
    pub scale: f32,
}

impl SpawnBatch {
    /// One particle of `system` at the emitter's origin, born at time 0, with seed 0: the starting point every caller
    /// adjusts.
    pub const fn new(system: ParticleSystemId, emitter: Mat4) -> Self {
        Self {
            system,
            emitter,
            shape: EmitterShape::Point,
            velocity: Vec3::ZERO,
            start: Seconds(0.0),
            interval: 0.0,
            first: 0,
            count: 1,
            seed: 0,
            cells: CellPick::All,
            colours: None,
            scale: 1.0,
        }
    }

    /// The time particle `g` is born, in scene seconds (f64, so long streams keep their precision).
    pub fn birth(&self, g: u32) -> Seconds {
        Seconds(self.start.0 + f64::from(g.wrapping_sub(self.first)) * f64::from(self.interval))
    }

    /// Checks the rules of D2 that need no resource tables.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: &str| Err(RenderError::invalid("spawn batch", reason.to_owned()));
        if !self.emitter.is_finite() || !self.velocity.is_finite() {
            return bad("emitter and velocity must be finite");
        }
        if !self.start.0.is_finite() {
            return bad("start must be finite");
        }
        if !(self.interval >= 0.0 && self.interval.is_finite()) {
            return bad("interval must be a finite number >= 0");
        }
        if !(self.scale > 0.0 && self.scale.is_finite()) {
            return bad("scale must be a finite number above 0");
        }
        let extent = self.shape.extent();
        if !(extent.is_finite() && extent.min_element() >= 0.0) {
            return bad("emitter shape sizes must be finite and >= 0");
        }
        if self.first.checked_add(self.count).is_none() {
            return bad("first + count overflows u32");
        }
        if let CellPick::List { len, .. } = self.cells
            && !(1..=8).contains(&len)
        {
            return bad("a cell list holds 1 to 8 cells");
        }
        Ok(())
    }
}

/// Checks a scene's batches: the limits and each batch's own rules ([`Scene::validate`](crate::Scene::validate)
/// calls it).
pub(crate) fn validate_batches(batches: &[SpawnBatch]) -> Result<(), RenderError> {
    if batches.len() > MAX_SPAWN_BATCHES {
        return Err(RenderError::LimitExceeded { what: "spawn batches in a scene", limit: MAX_SPAWN_BATCHES as u64 });
    }
    let mut slots = 0u64;
    for b in batches {
        b.validate()?;
        slots += u64::from(b.count);
    }
    if slots > u64::from(MAX_PARTICLES) {
        return Err(RenderError::LimitExceeded { what: "particles in a scene", limit: u64::from(MAX_PARTICLES) });
    }
    Ok(())
}

/// The random streams of [`particle_random`]: one fixed number per choice about a particle. New streams take new
/// numbers; the numbers never move, because moving one changes every golden.
pub struct Stream;

impl Stream {
    /// Lifetime.
    pub const LIFE: u32 = 0;
    /// Whether the density test keeps it.
    pub const KEEP: u32 = 1;
    /// Birth point, first coordinate.
    pub const POS0: u32 = 2;
    /// Birth point, second coordinate.
    pub const POS1: u32 = 3;
    /// Birth point, third coordinate.
    pub const POS2: u32 = 4;
    /// The angle off the emission axis.
    pub const DIR0: u32 = 5;
    /// The angle around the emission axis.
    pub const DIR1: u32 = 6;
    /// Speed.
    pub const SPEED: u32 = 7;
    /// Flutter phase.
    pub const PHASE: u32 = 8;
    /// Flutter direction.
    pub const SIDE: u32 = 9;
    /// Size.
    pub const SIZE: u32 = 10;
    /// Colour.
    pub const COLOUR: u32 = 11;
    /// The starting roll or tumble angle.
    pub const ROLL: u32 = 12;
    /// Spin or tumble rate.
    pub const SPIN: u32 = 13;
    /// Tumble axis, first coordinate.
    pub const AX0: u32 = 14;
    /// Tumble axis, second coordinate.
    pub const AX1: u32 = 15;
    /// Atlas cell.
    pub const CELL: u32 = 16;
}

/// The PCG hash of Jarzynski and Olano ("Hash Functions for GPU Rendering", JCGT 2020), restated from the paper.
pub const fn pcg(x: u32) -> u32 {
    let s = x.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277_803_737);
    (w >> 22) ^ w
}

/// A uniform number in `[0, 1)` for choice `stream` about particle `index` of a batch seeded `seed` (D3). Every value
/// is exactly representable in `f32`, so the CPU and the GPU agree bit for bit.
#[allow(clippy::cast_precision_loss)] // 24 bits fit an f32 exactly
pub const fn particle_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = pcg(pcg(pcg(index) ^ seed) ^ stream.wrapping_mul(0x9E37_79B9));
    (h >> 8) as f32 / 16_777_216.0
}

/// The branchless orthonormal basis of Duff et al. ("Building an Orthonormal Basis, Revisited", JCGT 2017) for a
/// unit vector `n`: two unit vectors that, with `n`, form a right-handed orthonormal frame.
pub fn onb(n: Vec3) -> (Vec3, Vec3) {
    let s = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    (Vec3::new(1.0 + s * n.x * n.x * a, s * b, -s * n.x), Vec3::new(b, s + n.y * n.y * a, -n.y))
}

/// A unit vector uniform on the sphere from two uniform numbers.
pub fn sphere_dir(u0: f32, u1: f32) -> Vec3 {
    let z = 1.0 - 2.0 * u0;
    let r = (1.0 - z * z).max(0.0).sqrt();
    let (s, c) = (TAU * u1).sin_cos();
    Vec3::new(r * c, r * s, z)
}

/// `floor(u n)` clamped to `n - 1`, for picking one of `n` things with a uniform `u`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
fn pick_index(u: f32, n: u32) -> u32 {
    ((u * n as f32) as u32).min(n.saturating_sub(1))
}

/// The size multiplier at life fraction `x` in `[0, 1]`: piecewise linear through `(0, c0), (1/3, c1), (2/3, c2),
/// (1, c3)`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
pub fn size_curve(curve: [f32; 4], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) * 3.0;
    let i = (x as usize).min(2);
    let f = x - i as f32;
    curve[i] + (curve[i + 1] - curve[i]) * f
}

/// One particle at one time: what a renderer needs to draw it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ParticleState {
    /// The centre, world space.
    pub position: Vec3,
    /// Metres per second, world space (the path's, without flutter).
    pub velocity: Vec3,
    /// The sprite's height in metres (a streak's width), after the size curve and the batch's scale.
    pub size: f32,
    /// The quad's width and height (along the velocity for [`Orientation::Velocity`]) in metres.
    pub extent: Vec2,
    /// The particle's colour in linear light, before lighting.
    pub colour: Vec3,
    /// A tumbling quad's normal; zero for a quad that faces the camera.
    pub normal: Vec3,
    /// A tumbling quad's edges (width, height), world space; zero for a quad that faces the camera.
    pub edges: [Vec3; 2],
    /// A billboard's roll in radians (the tumble angle for a tumbling quad, 0 along the velocity).
    pub roll: f32,
    /// The atlas cell (0 for a shape).
    pub cell: u32,
    /// Seconds since birth.
    pub age: f32,
    /// Seconds it lives.
    pub lifetime: f32,
}

/// Particle `index` of `batch` at scene time `time` (D3), or `None` when it is outside the batch, unborn, dead,
/// thinned by `density` (`0..=1`, [`Quality::particle_density`](crate::Quality::particle_density)) or below the
/// system's floor.
///
/// This is the one definition: the mock draws with it and the GPU shader is tested against it.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::many_single_char_names)]
pub fn evaluate(
    desc: &ParticleSystemDesc,
    batch: &SpawnBatch,
    index: u32,
    time: Seconds,
    density: f32,
) -> Option<ParticleState> {
    let p = &desc.params;
    let g = index;
    if g < batch.first || u64::from(g) >= u64::from(batch.first) + u64::from(batch.count) {
        return None;
    }
    let u = |k: u32| particle_random(batch.seed, g, k);
    // The long subtraction in f64, then f32 for everything that follows (D3).
    let age0 = (time.0 - batch.start.0) as f32;
    let a = age0 - (g - batch.first) as f32 * batch.interval;
    let lifetime = p.lifetime.at(u(Stream::LIFE));
    if !(a >= 0.0 && a < lifetime) || u(Stream::KEEP) >= density {
        return None;
    }

    let local = match batch.shape {
        EmitterShape::Point => Vec3::ZERO,
        EmitterShape::Sphere { radius } => {
            radius * u(Stream::POS0).cbrt() * sphere_dir(u(Stream::POS1), u(Stream::POS2))
        }
        EmitterShape::Box { half } => {
            Vec3::new(2.0 * u(Stream::POS0) - 1.0, 2.0 * u(Stream::POS1) - 1.0, 2.0 * u(Stream::POS2) - 1.0) * half
        }
        EmitterShape::Rect { half } => {
            Vec3::new((2.0 * u(Stream::POS0) - 1.0) * half.x, 0.0, (2.0 * u(Stream::POS1) - 1.0) * half.y)
        }
    };
    let p0 = batch.emitter.transform_point3(local);

    let axis = (Mat3::from_mat4(batch.emitter) * Vec3::from(p.direction)).normalize_or(Vec3::Y);
    let (t1, t2) = onb(axis);
    let cos_spread = p.spread_deg.to_radians().cos();
    let cos_t = 1.0 - u(Stream::DIR0) * (1.0 - cos_spread);
    let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
    let (sin_p, cos_p) = (TAU * u(Stream::DIR1)).sin_cos();
    let dir = t1 * (sin_t * cos_p) + t2 * (sin_t * sin_p) + axis * cos_t;
    let v0 = dir * p.speed.at(u(Stream::SPEED)) + batch.velocity;

    let gravity = Vec3::from(p.gravity);
    let (mut position, velocity) = if p.drag > 0.0 {
        let k = p.drag;
        let terminal = Vec3::from(p.wind) + gravity / k;
        let decay = (-k * a).exp();
        (p0 + terminal * a + (v0 - terminal) * ((1.0 - decay) / k), terminal + (v0 - terminal) * decay)
    } else {
        (p0 + v0 * a + gravity * (0.5 * a * a), v0 + gravity * a)
    };
    if let Some(f) = p.flutter {
        let sway = f.amplitude_m * (TAU * f.frequency_hz * a + TAU * u(Stream::PHASE)).sin();
        let (s, c) = (TAU * u(Stream::SIDE)).sin_cos();
        position += Vec3::new(c, 0.0, s) * sway;
    }
    if p.floor.is_some_and(|floor| position.y < floor) {
        return None;
    }

    let size = p.size.at(u(Stream::SIZE)) * size_curve(p.size_curve, a / lifetime) * batch.scale;
    let colours = batch.colours.as_ref().unwrap_or(&p.colours);
    let colour = colours.pick(u(Stream::COLOUR)).to_linear();
    let aspect = desc.aspect();
    let (extent, normal, edges, roll) = match p.orientation {
        Orientation::Billboard { spin_deg } => {
            let roll = TAU * u(Stream::ROLL) + spin_deg.at(u(Stream::SPIN)).to_radians() * a;
            (Vec2::new(size * aspect, size), Vec3::ZERO, [Vec3::ZERO; 2], roll)
        }
        Orientation::Velocity { stretch } => {
            (Vec2::new(size, size + stretch * velocity.length()), Vec3::ZERO, [Vec3::ZERO; 2], 0.0)
        }
        Orientation::Tumble { rate_deg } => {
            let n = sphere_dir(u(Stream::AX0), u(Stream::AX1));
            let (b1, b2) = onb(n);
            let theta = TAU * u(Stream::ROLL) + rate_deg.at(u(Stream::SPIN)).to_radians() * a;
            let (s, c) = theta.sin_cos();
            let e1 = b1 * (size * aspect);
            let e2 = (b2 * c + n * s) * size;
            (Vec2::new(size * aspect, size), n * c - b2 * s, [e1, e2], theta)
        }
    };
    let cell = match (desc.sprite, batch.cells) {
        (Sprite::Shape(_), _) => 0,
        (Sprite::Atlas { .. }, CellPick::List { cells, len }) => {
            u32::from(cells[pick_index(u(Stream::CELL), u32::from(len.clamp(1, 8))) as usize])
        }
        (Sprite::Atlas { .. }, CellPick::All) => pick_index(u(Stream::CELL), desc.cells()),
    };
    Some(ParticleState { position, velocity, size, extent, colour, normal, edges, roll, cell, age: a, lifetime })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_colour_sets_serialise_as_lists() {
        let s: Span = serde_json::from_str("[1.0, 2.0]").unwrap();
        assert_eq!(s, Span::new(1.0, 2.0));
        assert_eq!(serde_json::to_string(&s).unwrap(), "[1.0,2.0]");
        let c: ColourSet = serde_json::from_str(r##"["#ff0000", "#00ff00"]"##).unwrap();
        assert_eq!(c.as_slice(), [Colour::rgb(255, 0, 0), Colour::rgb(0, 255, 0)]);
        assert!(serde_json::from_str::<ColourSet>("[]").is_err(), "an empty set is refused");
        let nine = serde_json::json!(vec!["#000000"; 9]);
        assert!(serde_json::from_value::<ColourSet>(nine).is_err(), "nine is one too many");
    }

    #[test]
    fn the_curve_passes_through_its_four_points() {
        let c = [0.4, 1.0, 0.5, 0.0];
        for (x, want) in [(0.0, 0.4), (1.0 / 3.0, 1.0), (2.0 / 3.0, 0.5), (1.0, 0.0), (0.5, 0.75)] {
            assert!((size_curve(c, x) - want).abs() < 1e-5, "{x}: {}", size_curve(c, x));
        }
    }
}
