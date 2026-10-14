//! The pose data: [`Finger`], [`FingerCurl`], [`HandShape`], and how a shape is checked and blended.

use std::borrow::Cow;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::id::Slug;

use super::apply::Side;
use super::library::HandLibrary;

/// The widest swing of a finger either way, degrees. A fan or a V is well inside it; a value beyond it is a typo.
const SPREAD_LIMIT_DEG: f32 = 90.0;
/// How far a joint may be flexed toward the palm, degrees (a fist needs about 100).
const FLEX_MAX_DEG: f32 = 130.0;
/// How far a joint may bend back (hyperextension), degrees.
const FLEX_MIN_DEG: f32 = -45.0;

/// `t` held to `0..=1`, with a NaN counting as `0`: trackers and cues overshoot, and a NaN must never reach a bone.
pub(super) fn unit(t: f32) -> f32 {
    if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) }
}

/// A finger of one hand. The order is the order of [`HandShape::curls`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Finger {
    /// The thumb (`ThumbMetacarpal`, `ThumbProximal`, `ThumbDistal`).
    Thumb,
    /// The index finger (`IndexProximal`, `IndexIntermediate`, `IndexDistal`).
    Index,
    /// The middle finger.
    Middle,
    /// The ring finger.
    Ring,
    /// The little finger.
    Little,
}

impl Finger {
    /// The five fingers, thumb first.
    pub const ALL: [Finger; 5] = [Finger::Thumb, Finger::Index, Finger::Middle, Finger::Ring, Finger::Little];

    /// The lower-case name, as used in the library files.
    pub const fn name(self) -> &'static str {
        match self {
            Finger::Thumb => "thumb",
            Finger::Index => "index",
            Finger::Middle => "middle",
            Finger::Ring => "ring",
            Finger::Little => "little",
        }
    }

    /// The finger's three humanoid bones on `side`, base first: the knuckle bone, the middle one, the tip one. For the
    /// thumb these are the metacarpal, proximal and distal bones.
    pub const fn bones(self, side: Side) -> [HumanoidBone; 3] {
        use HumanoidBone as B;
        match side {
            Side::Left => match self {
                Finger::Thumb => [B::LeftThumbMetacarpal, B::LeftThumbProximal, B::LeftThumbDistal],
                Finger::Index => [B::LeftIndexProximal, B::LeftIndexIntermediate, B::LeftIndexDistal],
                Finger::Middle => [B::LeftMiddleProximal, B::LeftMiddleIntermediate, B::LeftMiddleDistal],
                Finger::Ring => [B::LeftRingProximal, B::LeftRingIntermediate, B::LeftRingDistal],
                Finger::Little => [B::LeftLittleProximal, B::LeftLittleIntermediate, B::LeftLittleDistal],
            },
            Side::Right => match self {
                Finger::Thumb => [B::RightThumbMetacarpal, B::RightThumbProximal, B::RightThumbDistal],
                Finger::Index => [B::RightIndexProximal, B::RightIndexIntermediate, B::RightIndexDistal],
                Finger::Middle => [B::RightMiddleProximal, B::RightMiddleIntermediate, B::RightMiddleDistal],
                Finger::Ring => [B::RightRingProximal, B::RightRingIntermediate, B::RightRingDistal],
                Finger::Little => [B::RightLittleProximal, B::RightLittleIntermediate, B::RightLittleDistal],
            },
        }
    }
}

/// One of the four angles of a [`FingerCurl`], named for error messages.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Joint {
    /// The swing in the palm plane.
    Spread,
    /// The knuckle (the thumb's metacarpal flexion).
    Mcp,
    /// The middle joint.
    Pip,
    /// The fingertip joint.
    Dip,
}

impl Joint {
    /// The key used in the library files.
    pub const fn name(self) -> &'static str {
        match self {
            Joint::Spread => "spread",
            Joint::Mcp => "mcp",
            Joint::Pip => "pip",
            Joint::Dip => "dip",
        }
    }

    /// The allowed range, degrees.
    const fn range_deg(self) -> (f32, f32) {
        match self {
            Joint::Spread => (-SPREAD_LIMIT_DEG, SPREAD_LIMIT_DEG),
            Joint::Mcp | Joint::Pip | Joint::Dip => (FLEX_MIN_DEG, FLEX_MAX_DEG),
        }
    }
}

/// The four angles of one finger, radians. See the module docs for the sign of each.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FingerCurl {
    /// Swing in the palm plane, positive toward the thumb side.
    pub spread: f32,
    /// Knuckle flexion, positive toward the palm.
    pub mcp: f32,
    /// Middle-joint flexion.
    pub pip: f32,
    /// Fingertip-joint flexion.
    pub dip: f32,
}

impl FingerCurl {
    /// The four angles in the order of [`Joint`], with their joints.
    fn angles(self) -> [(Joint, f32); 4] {
        [(Joint::Spread, self.spread), (Joint::Mcp, self.mcp), (Joint::Pip, self.pip), (Joint::Dip, self.dip)]
    }

    /// Each angle moved `t` (`0..=1`) of the way to `other`; exact at both ends, which `a + (b - a) * 1.0` is not.
    fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |a: f32, b: f32| {
            if t <= 0.0 {
                a
            } else if t >= 1.0 {
                b
            } else {
                a + (b - a) * t
            }
        };
        Self {
            spread: mix(self.spread, other.spread),
            mcp: mix(self.mcp, other.mcp),
            pip: mix(self.pip, other.pip),
            dip: mix(self.dip, other.dip),
        }
    }
}

/// Why a hand shape or the library could not be loaded.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum HandShapeError {
    /// A library file is not valid TOML of the expected layout.
    #[error("hand shape file {file}: {message}")]
    Parse {
        /// Which file (its position in the list, or its shape id when known).
        file: String,
        /// What the parser said.
        message: String,
    },
    /// The id is not a valid name (`a-z`, `0-9`, `-`, `_`).
    #[error("hand shape id {id:?} is not a valid name: {message}")]
    BadId {
        /// The id as written.
        id: String,
        /// Which rule it breaks.
        message: String,
    },
    /// The summary is empty; `hand.list_shapes` would show nothing.
    #[error("hand shape {id} has no summary")]
    EmptySummary {
        /// The shape.
        id: String,
    },
    /// An angle is not finite or is outside the range a hand can make.
    #[error("hand shape {id}: {finger} {joint} is {degrees} degrees, outside {min}..={max}")]
    Angle {
        /// The shape.
        id: String,
        /// The finger's name.
        finger: &'static str,
        /// The angle's name.
        joint: &'static str,
        /// The value found, degrees.
        degrees: f32,
        /// The smallest allowed, degrees.
        min: f32,
        /// The largest allowed, degrees.
        max: f32,
    },
    /// Two library files use the same id.
    #[error("two hand shapes are called {0}")]
    DuplicateId(String),
    /// A shape the code relies on is not in the library.
    #[error("the hand-shape library has no {0:?} shape")]
    Missing(&'static str),
}

/// A named hand pose: five finger curls, thumb first. Cheap to clone when the id is static (computed shapes), so
/// blending or tracking a hand every frame never allocates.
#[derive(Clone, Debug, PartialEq)]
pub struct HandShape {
    /// The library name (`fist`), or a stand-in for a computed shape (`curls`, `blend`).
    pub id: Cow<'static, str>,
    /// One line saying what the shape is, for `hand.list_shapes`.
    pub summary: Cow<'static, str>,
    /// The curl of each finger, in [`Finger::ALL`] order.
    pub curls: [FingerCurl; 5],
}

impl HandShape {
    /// The curl of one finger.
    pub fn curl(&self, finger: Finger) -> FingerCurl {
        self.curls[finger as usize]
    }

    /// Checks every angle is finite and within [`Joint::range_deg`], and the id and summary are usable.
    ///
    /// # Errors
    /// The first [`HandShapeError`] found.
    pub fn validate(&self) -> Result<(), HandShapeError> {
        Slug::new(self.id.as_ref())
            .map_err(|e| HandShapeError::BadId { id: self.id.to_string(), message: e.to_string() })?;
        if self.summary.trim().is_empty() {
            return Err(HandShapeError::EmptySummary { id: self.id.to_string() });
        }
        for finger in Finger::ALL {
            for (joint, radians) in self.curl(finger).angles() {
                let (min, max) = joint.range_deg();
                let degrees = radians.to_degrees();
                if !(min..=max).contains(&degrees) {
                    let id = self.id.to_string();
                    return Err(HandShapeError::Angle {
                        id,
                        finger: finger.name(),
                        joint: joint.name(),
                        degrees,
                        min,
                        max,
                    });
                }
            }
        }
        Ok(())
    }

    /// A shape between two others: every angle moved `t` of the way from `self` to `other`, with `t` held to `0..=1`
    /// (a NaN counts as `0`). Exact at both ends. The result is a stand-in named `blend`.
    ///
    /// Angles are mixed linearly, not as rotations: every angle is a single-axis bend, so the two agree, and a
    /// blend of two valid shapes is itself valid.
    #[must_use]
    pub fn blend(&self, other: &Self, t: f32) -> Self {
        let t = unit(t);
        let mut curls = self.curls;
        for (c, o) in curls.iter_mut().zip(&other.curls) {
            *c = c.lerp(*o, t);
        }
        Self { id: Cow::Borrowed("blend"), summary: Cow::Borrowed("A blend of two hand shapes."), curls }
    }

    /// A shape from one `0..=1` curl per finger, thumb first, as hand tracking reports them: `0` is that finger as in
    /// the `open` shape, `1` as in the `fist`, and values between mix the two. Values outside `0..=1` are held to it
    /// (a NaN counts as `0`), because trackers overshoot. The result is a stand-in named `curls`.
    ///
    /// # Errors
    /// [`HandShapeError`] if the built-in library failed to load or lacks `open` or `fist`; this cannot happen in a
    /// build that passes its tests.
    pub fn from_curls(curls: [f32; 5]) -> Result<Self, HandShapeError> {
        let (open, fist) = HandLibrary::tracking_anchors()?;
        // Built from the anchors' angles, not cloned from them: their ids are owned strings, and this runs per frame.
        let mut mixed = open.curls;
        for ((out, c), b) in mixed.iter_mut().zip(curls).zip(&fist.curls) {
            *out = out.lerp(*b, unit(c));
        }
        Ok(Self {
            id: Cow::Borrowed("curls"),
            summary: Cow::Borrowed("Finger curls from a tracker, between open and fist."),
            curls: mixed,
        })
    }
}
