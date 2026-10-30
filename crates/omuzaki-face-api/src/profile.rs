//! The face profile: everything that makes one character's painted face *theirs* (spec CB-12, CB-20..23, FE-06), stored
//! as `face.toml` (§7). Channels say what the face does *now*; the profile says what the face *is* and how it reads them.
//!
//! Units: lengths are metres in **head-local face-plane coordinates** (origin: the head centre, +x toward the
//! character's left, +y up). Angles are unitless tilt factors, counts are plain numbers. Every struct has defaults,
//! so a profile file lists only what differs from a generic anime face, and rejects unknown keys (typos fail loudly).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use omuzaki_anim_api::ChannelBuf;

use crate::check::{Check, Validate};
use crate::expression::{ChannelValues, ExpressionId, ExpressionKind, ExpressionSet};
use crate::eye3d::EyeDesign;
use crate::grammar::GrammarRules;
use crate::mapping::FaceMapping;
use crate::palette::ColourRef;

mod eye_detail;
mod makeup;

pub use makeup::{EyeShadow, TeethStyle, TongueStyle};

pub use eye_detail::{
    Catchlight, Collarette, IrisDetail, IrisFibres, IrisFlecks, LashStrands, LimbalRing, MAX_CATCHLIGHTS, StrandRow,
};

const fn rgb(r: u8, g: u8, b: u8) -> ColourRef {
    ColourRef::rgb(r, g, b)
}

// ------------------------------------------------------------------------------------------------ layout

/// Where the features sit on the face (CB-12). All heights are relative to the head centre, so they follow the head
/// shape profile when it changes; [`FaceLayout::ratios`] gives the head-size-independent view the overlay shows.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FaceLayout {
    /// Height of the eye line (negative: below the head centre).
    pub eye_y: f64,
    /// Half the distance between the eye centres.
    pub eye_spacing: f64,
    /// Half the width of an eye at scale 1.
    pub eye_width: f64,
    /// Height of the brows above the eye line.
    pub brow_gap: f64,
    /// Height of the nose mark.
    pub nose_y: f64,
    /// Height of the mouth line.
    pub mouth_y: f64,
    /// Height of the centre of the blush patches.
    pub blush_y: f64,
    /// Half the distance between the blush patch centres.
    pub blush_spacing: f64,
}

impl Default for FaceLayout {
    fn default() -> Self {
        Self {
            eye_y: -0.09,
            eye_spacing: 0.088,
            eye_width: 0.05,
            brow_gap: 0.08,
            nose_y: -0.115,
            mouth_y: -0.15,
            blush_y: -0.13,
            blush_spacing: 0.1,
        }
    }
}

/// The layout as ratios, independent of absolute head size (CB-12's overlay and the report card).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FaceRatios {
    /// Half an eye's width ([`FaceLayout::eye_width`]) over the distance between the eye centres. A full drawn eye width
    /// is twice the numerator.
    pub eye_width_to_iod: f64,
    /// Where the nose sits between the eye line (0) and the chin (1).
    pub nose: f64,
    /// Where the mouth sits between the eye line (0) and the chin (1).
    pub mouth: f64,
    /// Eye-line-to-chin drop over the distance between the eye centres.
    pub drop_to_iod: f64,
}

impl FaceLayout {
    /// The ratios, given the chin height (head-local, from the head shape). `None` if the chin is not below the
    /// eye line or the eyes coincide.
    pub fn ratios(&self, chin_y: f64) -> Option<FaceRatios> {
        let drop = self.eye_y - chin_y;
        let iod = 2.0 * self.eye_spacing;
        (drop > 0.0 && iod > 0.0).then(|| FaceRatios {
            eye_width_to_iod: self.eye_width / iod,
            nose: (self.eye_y - self.nose_y) / drop,
            mouth: (self.eye_y - self.mouth_y) / drop,
            drop_to_iod: drop / iod,
        })
    }

    /// Linear blend of two layouts (profile blending, CB-10).
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        let m = |a: f64, b: f64| a + (b - a) * t;
        Self {
            eye_y: m(self.eye_y, other.eye_y),
            eye_spacing: m(self.eye_spacing, other.eye_spacing),
            eye_width: m(self.eye_width, other.eye_width),
            brow_gap: m(self.brow_gap, other.brow_gap),
            nose_y: m(self.nose_y, other.nose_y),
            mouth_y: m(self.mouth_y, other.mouth_y),
            blush_y: m(self.blush_y, other.blush_y),
            blush_spacing: m(self.blush_spacing, other.blush_spacing),
        }
    }
}

impl Validate for FaceLayout {
    fn check(&self, c: &mut Check) {
        c.range("eye_y", self.eye_y, -0.3, 0.3);
        c.range("eye_spacing", self.eye_spacing, 0.02, 0.2);
        c.range("eye_width", self.eye_width, 0.01, 0.12);
        c.range("brow_gap", self.brow_gap, 0.0, 0.15);
        c.range("nose_y", self.nose_y, -0.4, 0.3);
        c.range("mouth_y", self.mouth_y, -0.4, 0.3);
        c.range("blush_y", self.blush_y, -0.4, 0.3);
        c.range("blush_spacing", self.blush_spacing, 0.0, 0.25);
        c.rule(&["mouth_y", "eye_y"], self.mouth_y < self.eye_y, "the mouth must be below the eye line");
    }
}

// ------------------------------------------------------------------------------------------------ eyes

/// Shape of the eye opening (CB-20), in units of the eye's half-width.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Aperture {
    /// 0 a flat almond, 1 a round eye.
    pub roundness: f64,
    /// Resting tilt of the lids: positive lifts the outer corner (cat-eye), negative drops it.
    pub tilt: f64,
    /// How pointed the inner corner is, 0 rounded to 1 sharp.
    pub inner_sharpness: f64,
    /// How blunt the outer corner is, 0 pointed to 1 rounded.
    pub outer_bluntness: f64,
    /// Height of the upper lid at the eye's centre when `eye_open` is 1.
    pub upper_height: f64,
    /// Depth of the lower lid curve below the eye's centre line.
    pub lower_depth: f64,
}

impl Default for Aperture {
    fn default() -> Self {
        Self {
            roundness: 0.2,
            tilt: 0.0,
            inner_sharpness: 0.8,
            outer_bluntness: 0.6,
            upper_height: 0.4,
            lower_depth: 0.55,
        }
    }
}

impl Validate for Aperture {
    fn check(&self, c: &mut Check) {
        c.range("roundness", self.roundness, 0.0, 1.0);
        c.range("tilt", self.tilt, -1.0, 1.0);
        c.range("inner_sharpness", self.inner_sharpness, 0.0, 1.0);
        c.range("outer_bluntness", self.outer_bluntness, 0.0, 1.0);
        c.range("upper_height", self.upper_height, 0.1, 1.0);
        c.range("lower_depth", self.lower_depth, 0.1, 1.0);
    }
}

/// The lash line (CB-20).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lash {
    /// Line thickness at the outer end, in metres.
    pub thickness: f64,
    /// Length of the lifted outer flick, 0 none to 1 long.
    pub flick: f64,
    /// Strength of the small lash under the outer corner, 0 none.
    pub lower: f64,
    /// A short warm tick at the inner corner.
    pub inner_tick: bool,
    /// Line colour.
    pub colour: ColourRef,
    /// Strand lashes along both lids, in the line's colour (#2468); none by default.
    pub strands: LashStrands,
}

impl Default for Lash {
    fn default() -> Self {
        Self {
            thickness: 0.006,
            flick: 0.5,
            lower: 0.5,
            inner_tick: true,
            colour: rgb(0x3a, 0x24, 0x20),
            strands: LashStrands::default(),
        }
    }
}

impl Validate for Lash {
    fn check(&self, c: &mut Check) {
        c.range("thickness", self.thickness, 0.0005, 0.02);
        c.range("flick", self.flick, 0.0, 1.0);
        c.range("lower", self.lower, 0.0, 1.0);
        c.nested("strands", &self.strands);
    }
}

/// The crease above the lid (CB-20).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Crease {
    /// Opacity, 0 none to 1 solid.
    pub strength: f64,
    /// Distance above the upper lid, in eye half-widths.
    pub lift: f64,
    /// Line colour.
    pub colour: ColourRef,
}

impl Default for Crease {
    fn default() -> Self {
        Self { strength: 0.55, lift: 0.3, colour: rgb(0xaa, 0x68, 0x5c) }
    }
}

impl Validate for Crease {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("lift", self.lift, 0.05, 0.8);
    }
}

/// The shape of the pupil (CB-20).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PupilShape {
    /// A soft dark rounded slot (the default).
    Round,
    /// A vertical slit (cat, dragon).
    Slit,
    /// A heart.
    Heart,
    /// No pupil: a plain coloured iris.
    None,
}

/// The pupil.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Pupil {
    /// Shape.
    pub shape: PupilShape,
    /// Size relative to the iris; the `pupil_size` channel scales it further.
    pub size: f64,
    /// Colour, usually translucent.
    pub colour: ColourRef,
}

impl Default for Pupil {
    fn default() -> Self {
        Self { shape: PupilShape::Round, size: 1.0, colour: ColourRef::rgba(24, 52, 42, 140) }
    }
}

impl Validate for Pupil {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.2, 2.0);
    }
}

/// The iris (CB-20): a vertical gradient with a ring and a lit crescent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Iris {
    /// Half-width as a fraction of the eye's half-width.
    pub width: f64,
    /// Half-height as a fraction of the eye's half-width (taller than wide reads as anime).
    pub height: f64,
    /// How far the iris sits toward the nose from the eye centre, in eye half-widths (negative: outward).
    pub inset: f64,
    /// How far below the eye centre the iris sits, in eye half-widths.
    pub drop: f64,
    /// Top of the gradient.
    pub top: ColourRef,
    /// Middle of the gradient.
    pub mid: ColourRef,
    /// Bottom of the gradient (the lit crescent).
    pub low: ColourRef,
    /// The outline ring.
    pub ring: ColourRef,
    /// How far the iris travels per unit of `gaze_x` and `gaze_y`, in metres at eye scale 1.
    pub gaze_travel: [f64; 2],
    /// The pupil.
    pub pupil: Pupil,
    /// The iris's structure: fibres, collarette, flecks, limbal ring (#2465); none by default.
    pub detail: IrisDetail,
}

impl Default for Iris {
    fn default() -> Self {
        Self {
            width: 0.64,
            height: 0.84,
            inset: 0.1,
            drop: 0.07,
            top: rgb(0x2c, 0x4f, 0x45),
            mid: rgb(0x5e, 0x8f, 0x78),
            low: rgb(0xa8, 0xc9, 0x9c),
            ring: rgb(0x1d, 0x36, 0x2c),
            gaze_travel: [0.026, 0.013],
            pupil: Pupil::default(),
            detail: IrisDetail::default(),
        }
    }
}

impl Validate for Iris {
    fn check(&self, c: &mut Check) {
        c.range("width", self.width, 0.2, 1.0);
        c.range("height", self.height, 0.2, 1.4);
        c.range("inset", self.inset, -0.4, 0.5);
        c.range("drop", self.drop, -0.4, 0.5);
        c.range("gaze_travel/0", self.gaze_travel[0], 0.0, 0.08);
        c.range("gaze_travel/1", self.gaze_travel[1], 0.0, 0.08);
        c.nested("pupil", &self.pupil);
        c.nested("detail", &self.detail);
    }
}

/// The right eye's own iris colours and pupil, for a character whose eyes differ (heterochromia). Each field left
/// out is the left eye's ([`EyeStyle::iris`]), so `right_iris = { mid = "#c03a3a" }` changes one colour and nothing else.
/// The iris's size and place stay shared: the two eyes are one design.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct IrisColours {
    /// Top of the gradient.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<ColourRef>,
    /// Middle of the gradient.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mid: Option<ColourRef>,
    /// Bottom of the gradient (the lit crescent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<ColourRef>,
    /// The outline ring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ring: Option<ColourRef>,
    /// The pupil (its shape, size and colour), when it differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pupil: Option<Pupil>,
}

impl Validate for IrisColours {
    fn check(&self, c: &mut Check) {
        if let Some(pupil) = &self.pupil {
            c.nested("pupil", pupil);
        }
    }
}

/// The colours and pupil one eye's iris paints with: the left eye's [`Iris`], or for the right eye the same with the
/// [`EyeStyle::right_iris`] overrides on top. Borrowed, so painting a frame copies nothing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct IrisPaint<'a> {
    /// Top of the gradient.
    pub top: &'a ColourRef,
    /// Middle of the gradient.
    pub mid: &'a ColourRef,
    /// Bottom of the gradient.
    pub low: &'a ColourRef,
    /// The ring.
    pub ring: &'a ColourRef,
    /// The pupil.
    pub pupil: &'a Pupil,
}

/// The eye white, shaded under the lid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Sclera {
    /// Colour under the upper lid.
    pub top: ColourRef,
    /// Colour in the middle.
    pub mid: ColourRef,
    /// Colour at the lower lid.
    pub bottom: ColourRef,
    /// How much the white darkens toward both corners, where the eyeball turns away, 0 none (#2467).
    pub corner_shade: f64,
    /// The corners' shade colour.
    pub corner_colour: ColourRef,
    /// Fine vessels running in from the corners, 0 none.
    pub vessels: f64,
    /// The vessels' colour.
    pub vessel_colour: ColourRef,
    /// The pink caruncle in the inner corner, 0 none.
    pub caruncle: f64,
    /// The caruncle's colour.
    pub caruncle_colour: ColourRef,
    /// The wet waterline along the lower lid: a soft rim of the lid's colour with a thin bright line on it, 0 none.
    pub waterline: f64,
    /// The waterline's rim colour.
    pub waterline_colour: ColourRef,
}

impl Default for Sclera {
    fn default() -> Self {
        Self {
            top: rgb(0xe6, 0xc9, 0xcc),
            mid: rgb(0xfb, 0xf3, 0xf1),
            bottom: rgb(0xff, 0xfa, 0xf8),
            corner_shade: 0.0,
            corner_colour: rgb(0xc9, 0xae, 0xb4),
            vessels: 0.0,
            vessel_colour: rgb(0xd4, 0x6a, 0x6a),
            caruncle: 0.0,
            caruncle_colour: rgb(0xe8, 0x9a, 0x96),
            waterline: 0.0,
            waterline_colour: rgb(0xe6, 0xa8, 0xa0),
        }
    }
}

impl Validate for Sclera {
    fn check(&self, c: &mut Check) {
        c.range("corner_shade", self.corner_shade, 0.0, 1.0);
        c.range("vessels", self.vessels, 0.0, 1.0);
        c.range("caruncle", self.caruncle, 0.0, 1.0);
        c.range("waterline", self.waterline, 0.0, 1.0);
    }
}

/// The shape of the main catchlight (CB-20). The `eye_heart` and `eye_star` channels swap to their own shape.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CatchlightShape {
    /// A glossy blob.
    Blob,
    /// A four-point star.
    Star,
    /// A heart.
    Heart,
    /// A window: a rounded pane split in four by a cross bar, the reflection of a lit room.
    Window,
    /// A thin ring, the reflection of a ring light.
    Ring,
    /// A crescent, opening away from the iris centre.
    Crescent,
    /// No catchlight.
    None,
}

/// Catchlights and shimmer (CB-20).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Highlights {
    /// Shape of the main catchlight.
    pub shape: CatchlightShape,
    /// Size of the main catchlight as a fraction of the iris.
    pub size: f64,
    /// Where it sits, in iris radii from the iris centre (x toward the character's left, y up).
    pub offset: [f64; 2],
    /// Strength of the small second catchlight, 0 none.
    pub second: f64,
    /// Wet-eye shimmer: a soft band of light gliding across the iris and twinkling stars, 0 none.
    pub shimmer: f64,
    /// How far reflections slide as the view direction changes, 0 none.
    pub view_motion: f64,
    /// More catchlights, drawn after the main one (#2466); at most [`MAX_CATCHLIGHTS`].
    pub extra: Vec<Catchlight>,
    /// A soft glow of light in the iris opposite the main catchlight, where light focused by the cornea lands, 0 none.
    pub caustic: f64,
    /// The glow's colour.
    pub caustic_colour: ColourRef,
}

impl Default for Highlights {
    fn default() -> Self {
        Self {
            shape: CatchlightShape::Blob,
            size: 0.3,
            offset: [0.28, 0.22],
            second: 0.65,
            shimmer: 0.0,
            view_motion: 0.5,
            extra: Vec::new(),
            caustic: 0.0,
            caustic_colour: ColourRef::rgba(255, 248, 225, 255),
        }
    }
}

impl Validate for Highlights {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.0, 1.0);
        c.range("offset/0", self.offset[0], -1.0, 1.0);
        c.range("offset/1", self.offset[1], -1.0, 1.0);
        c.range("second", self.second, 0.0, 1.0);
        c.range("shimmer", self.shimmer, 0.0, 1.0);
        c.range("view_motion", self.view_motion, 0.0, 1.0);
        c.rule(&["extra"], self.extra.len() <= MAX_CATCHLIGHTS, "an eye holds at most 8 extra catchlights");
        for (i, light) in self.extra.iter().enumerate() {
            c.nested(&format!("extra/{i}"), light);
        }
        c.range("caustic", self.caustic, 0.0, 1.0);
    }
}

/// The lower lid line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LowerLidStyle {
    /// Opacity of the fine line along the outer half of the lower lid.
    pub line: f64,
    /// A raised lower lid draws a full curved lash line with a soft rim (smiling eyes in close-ups).
    pub cheek_line: bool,
    /// Line colour.
    pub colour: ColourRef,
}

impl Default for LowerLidStyle {
    fn default() -> Self {
        Self { line: 0.65, cheek_line: false, colour: rgb(0x78, 0x46, 0x42) }
    }
}

impl Validate for LowerLidStyle {
    fn check(&self, c: &mut Check) {
        c.range("line", self.line, 0.0, 1.0);
    }
}

/// The eye designer (CB-20).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EyeStyle {
    /// Overall eye size multiplier (spacing scales by a third of it).
    pub scale: f64,
    /// How far the eyes sit lower than the layout's eye line, in metres (close-up faces).
    pub drop: f64,
    /// The opening.
    pub aperture: Aperture,
    /// The lash line.
    pub lash: Lash,
    /// The crease.
    pub crease: Crease,
    /// The iris (both eyes', unless `right_iris` says otherwise for the right one).
    pub iris: Iris,
    /// The right eye's own iris colours and pupil (heterochromia); left out, both eyes match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_iris: Option<IrisColours>,
    /// The eye white.
    pub sclera: Sclera,
    /// Catchlights.
    pub highlights: Highlights,
    /// The lower lid line.
    pub lower_lid: LowerLidStyle,
    /// Opacity of the shadow the upper lid casts over the iris.
    pub lid_shadow: f64,
    /// Colour of the `tear` drop.
    pub tear: ColourRef,
    /// Seeds the eye's fine detail (fibres, flecks, vessels, lashes), so one design always paints the same eye.
    pub seed: u32,
    /// Eyeshadow on the lid (#2472); none by default.
    pub shadow: EyeShadow,
}

impl Default for EyeStyle {
    fn default() -> Self {
        Self {
            scale: 1.0,
            drop: 0.0,
            aperture: Aperture::default(),
            lash: Lash::default(),
            crease: Crease::default(),
            iris: Iris::default(),
            right_iris: None,
            sclera: Sclera::default(),
            highlights: Highlights::default(),
            lower_lid: LowerLidStyle::default(),
            lid_shadow: 0.55,
            tear: ColourRef::rgba(190, 225, 255, 217),
            seed: 1,
            shadow: EyeShadow::default(),
        }
    }
}

impl Validate for EyeStyle {
    fn check(&self, c: &mut Check) {
        c.range("scale", self.scale, 0.5, 2.0);
        c.range("drop", self.drop, -0.05, 0.05);
        c.nested("aperture", &self.aperture);
        c.nested("lash", &self.lash);
        c.nested("crease", &self.crease);
        c.nested("iris", &self.iris);
        if let Some(right) = &self.right_iris {
            c.nested("right_iris", right);
        }
        c.nested("sclera", &self.sclera);
        c.nested("highlights", &self.highlights);
        c.nested("lower_lid", &self.lower_lid);
        c.range("lid_shadow", self.lid_shadow, 0.0, 1.0);
        c.nested("shadow", &self.shadow);
    }
}

impl EyeStyle {
    /// The iris colours and pupil of one eye: `right` is the character's right eye, which takes
    /// [`right_iris`](Self::right_iris) where it says something, and the shared iris everywhere else.
    pub fn iris_paint(&self, right: bool) -> IrisPaint<'_> {
        fn pick<'a>(own: Option<&'a Option<ColourRef>>, shared: &'a ColourRef) -> &'a ColourRef {
            own.and_then(Option::as_ref).unwrap_or(shared)
        }
        let iris = &self.iris;
        let over = if right { self.right_iris.as_ref() } else { None };
        IrisPaint {
            top: pick(over.map(|o| &o.top), &iris.top),
            mid: pick(over.map(|o| &o.mid), &iris.mid),
            low: pick(over.map(|o| &o.low), &iris.low),
            ring: pick(over.map(|o| &o.ring), &iris.ring),
            pupil: over.and_then(|o| o.pupil.as_ref()).unwrap_or(&iris.pupil),
        }
    }
}

// ------------------------------------------------------------------------------------------------ brows

/// The brow designer (CB-21). Brows are always also painted to the `Brows` layer, so hair can draw them through the
/// bangs when `through_hair` is set (the anime convention).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BrowStyle {
    /// Distance of the inner end from the midline.
    pub inner_x: f64,
    /// Distance of the outer end from the midline.
    pub outer_x: f64,
    /// Largest half-thickness, in metres.
    pub thickness: f64,
    /// Height of the arch in the middle, in metres.
    pub arch: f64,
    /// Brow colour.
    pub colour: ColourRef,
    /// Draw the brows over the hair (see-through bangs).
    pub through_hair: bool,
}

impl Default for BrowStyle {
    fn default() -> Self {
        Self {
            inner_x: 0.044,
            outer_x: 0.11,
            thickness: 0.0018,
            arch: 0.004,
            colour: ColourRef::rgba(168, 112, 96, 217),
            through_hair: true,
        }
    }
}

impl Validate for BrowStyle {
    fn check(&self, c: &mut Check) {
        c.range("inner_x", self.inner_x, 0.0, 0.15);
        c.range("outer_x", self.outer_x, 0.02, 0.25);
        c.range("thickness", self.thickness, 0.0003, 0.01);
        c.range("arch", self.arch, 0.0, 0.02);
        c.rule(
            &["outer_x", "inner_x"],
            self.outer_x > self.inner_x,
            "the outer end must be farther from the midline than the inner end",
        );
    }
}

// ------------------------------------------------------------------------------------------------ mouth

/// Lip rendering (CB-22).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lips {
    /// 0 a plain line (the flat sheet look), 1 a lip rim and a glossy lower lip (close-ups).
    pub strength: f64,
    /// Rim and gloss tint.
    pub colour: ColourRef,
    /// Lip colour (makeup, #2472): both lips washed with `colour` round a closed mouth, 0 none.
    pub tint: f64,
}

impl Default for Lips {
    fn default() -> Self {
        Self { strength: 0.0, colour: rgb(0xee, 0x8e, 0x92), tint: 0.0 }
    }
}

impl Validate for Lips {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("tint", self.tint, 0.0, 1.0);
    }
}

/// Colours inside an open mouth.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MouthInterior {
    /// Colour at the top.
    pub top: ColourRef,
    /// Colour at the bottom.
    pub bottom: ColourRef,
    /// The tongue resting inside.
    pub tongue: ColourRef,
    /// Upper teeth.
    pub teeth: ColourRef,
    /// Lower teeth.
    pub teeth_lower: ColourRef,
}

impl Default for MouthInterior {
    fn default() -> Self {
        Self {
            top: rgb(0x4a, 0x16, 0x22),
            bottom: rgb(0x6e, 0x24, 0x33),
            tongue: rgb(0xe0, 0x78, 0x7a),
            teeth: rgb(0xff, 0xfa, 0xf6),
            teeth_lower: rgb(0xf4, 0xec, 0xe8),
        }
    }
}

impl Validate for MouthInterior {
    fn check(&self, _: &mut Check) {}
}

/// Which fangs the character has (CB-22).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FangKind {
    /// No fangs: none are painted whatever the `fangs` channel says, and the `grin-fangs` rule writes nothing.
    None,
    /// A small pointed fang at both upper mouth corners (the default). Files written before single fangs say
    /// `pointed`, which still reads as this.
    #[serde(alias = "pointed")]
    Both,
    /// One fang, at the character's left corner (the viewer's right).
    #[serde(alias = "one-left")]
    OneLeft,
    /// One fang, at the character's right corner (the viewer's left).
    #[serde(alias = "one-right")]
    OneRight,
}

impl FangKind {
    /// The corners that have a fang, as signs along the mouth: `+1` the character's left, `-1` the right.
    pub fn corners(self) -> &'static [f32] {
        match self {
            Self::None => &[],
            Self::Both => &[-1.0, 1.0],
            Self::OneLeft => &[1.0],
            Self::OneRight => &[-1.0],
        }
    }
}

/// Fangs at the upper mouth corners (yaeba); drawn when the `fangs` channel is above 0 and the kind is not
/// [`FangKind::None`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FangStyle {
    /// Whether this character has fangs.
    pub kind: FangKind,
    /// Width of each fang's base, in metres.
    pub width: f64,
    /// Length, in metres.
    pub height: f64,
    /// Distance from the mouth centre, as a fraction of the mouth's half-width.
    pub position: f64,
}

impl Default for FangStyle {
    fn default() -> Self {
        Self { kind: FangKind::Both, width: 0.006, height: 0.0058, position: 0.72 }
    }
}

impl Validate for FangStyle {
    fn check(&self, c: &mut Check) {
        c.range("width", self.width, 0.0005, 0.01);
        c.range("height", self.height, 0.0005, 0.02);
        c.range("position", self.position, 0.3, 1.0);
    }
}

/// The mouth designer (CB-22).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MouthStyle {
    /// Half-width of the closed mouth at `mouth_wide` 1, in metres.
    pub half_width: f64,
    /// Size multiplier while closed: close-up faces need a readable mouth.
    pub closed_scale: f64,
    /// Size multiplier once open: anime mouths grow when they open.
    pub open_scale: f64,
    /// Closed line thickness, in metres.
    pub line_width: f64,
    /// Closed line colour.
    pub line_colour: ColourRef,
    /// How strongly smile and frown curve the line, in metres of corner travel per unit.
    pub corner_curve: f64,
    /// Lips.
    pub lips: Lips,
    /// Inside the mouth.
    pub interior: MouthInterior,
    /// Fangs.
    pub fang: FangStyle,
    /// Size multiplier in a wide shot, where the head is at most [`WIDE_SHOT`] of the frame's height: a mouth drawn
    /// for a close-up shrinks to a few pixels in a full-body shot, so lip sync stops reading. It eases back to 1 by
    /// [`CLOSE_SHOT`] ([`shot_scale`](Self::shot_scale)).
    pub wide_shot_scale: f64,
    /// The upper teeth's shape (#2473).
    pub teeth: TeethStyle,
    /// The tongue's shape (#2473).
    pub tongue: TongueStyle,
}

/// The head's share of the frame's height at or below which a shot is wide: a full-body shot.
pub const WIDE_SHOT: f32 = 0.12;
/// The share at or above which a shot is close: a bust shot and nearer, where the mouth is drawn at its own size.
pub const CLOSE_SHOT: f32 = 0.3;

impl MouthStyle {
    /// The mouth's size multiplier for a shot whose head covers `head_coverage` of the frame's height:
    /// [`wide_shot_scale`](Self::wide_shot_scale) at [`WIDE_SHOT`] and below, 1 at [`CLOSE_SHOT`] and above, and a
    /// smoothstep between, so a dolly in or out never jumps. 1 when the coverage is unknown.
    #[must_use]
    pub fn shot_scale(&self, head_coverage: Option<f32>) -> f32 {
        let Some(coverage) = head_coverage.filter(|c| c.is_finite() && *c > 0.0) else { return 1.0 };
        let t = ((CLOSE_SHOT - coverage) / (CLOSE_SHOT - WIDE_SHOT)).clamp(0.0, 1.0);
        let ease = t * t * (3.0 - 2.0 * t);
        1.0 + (crate::narrow(self.wide_shot_scale) - 1.0) * ease
    }
}

impl Default for MouthStyle {
    fn default() -> Self {
        Self {
            half_width: 0.0105,
            closed_scale: 1.0,
            open_scale: 1.0,
            line_width: 0.0016,
            line_colour: rgb(0xa8, 0x5a, 0x55),
            corner_curve: 0.0045,
            lips: Lips::default(),
            interior: MouthInterior::default(),
            fang: FangStyle::default(),
            wide_shot_scale: 1.4,
            teeth: TeethStyle::default(),
            tongue: TongueStyle::default(),
        }
    }
}

impl Validate for MouthStyle {
    fn check(&self, c: &mut Check) {
        c.range("half_width", self.half_width, 0.002, 0.05);
        c.range("closed_scale", self.closed_scale, 0.5, 4.0);
        c.range("open_scale", self.open_scale, 0.5, 4.0);
        c.range("line_width", self.line_width, 0.0003, 0.01);
        c.range("corner_curve", self.corner_curve, 0.0, 0.02);
        c.range("wide_shot_scale", self.wide_shot_scale, 1.0, 2.5);
        c.nested("lips", &self.lips);
        c.nested("interior", &self.interior);
        c.nested("fang", &self.fang);
        c.nested("teeth", &self.teeth);
        c.nested("tongue", &self.tongue);
    }
}

// ------------------------------------------------------------------------------------------------ nose, cheeks, marks

/// How the nose is drawn (CB-23).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoseKind {
    /// Not drawn.
    None,
    /// A tiny dot.
    Dot,
    /// A short tick with a dot of light above it.
    Tick,
}

/// The nose mark (CB-23).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct NoseStyle {
    /// Style.
    pub kind: NoseKind,
    /// Size multiplier.
    pub size: f64,
    /// Mark colour.
    pub colour: ColourRef,
    /// Contour (makeup, #2472): a soft shade down the shadow side of the nose, 0 none.
    pub shadow: f64,
    /// A soft light spot on the bridge and the tip, 0 none.
    pub highlight: f64,
}

impl Default for NoseStyle {
    fn default() -> Self {
        Self { kind: NoseKind::None, size: 1.0, colour: rgb(0xbe, 0x78, 0x78), shadow: 0.0, highlight: 0.0 }
    }
}

impl Validate for NoseStyle {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.2, 3.0);
        c.range("shadow", self.shadow, 0.0, 1.0);
        c.range("highlight", self.highlight, 0.0, 1.0);
    }
}

/// How blush is drawn (CB-23).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlushKind {
    /// Soft radial patches.
    Soft,
    /// Diagonal hatch lines (manga).
    Hatched,
    /// Soft patches swept up toward the temples.
    Sweep,
    /// One soft band across the nose from cheek to cheek.
    Bridge,
}

/// Blush and the warm wash over the lower face (CB-23). The `blush` channel scales `strength`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CheekStyle {
    /// Soft or hatched.
    pub kind: BlushKind,
    /// Blush colour.
    pub colour: ColourRef,
    /// Patch half-width and half-height, in metres.
    pub radius: [f64; 2],
    /// Opacity at channel value 1.
    pub strength: f64,
    /// Opacity of the warm wash over the lower face.
    pub wash: f64,
    /// Blush as makeup (#2472): opacity of blush that shows whatever the `blush` channel says, 0 none.
    pub base: f64,
    /// Contour: a soft shade under the cheekbones toward the jaw, 0 none.
    pub contour: f64,
    /// The contour's colour.
    pub contour_colour: ColourRef,
}

impl Default for CheekStyle {
    fn default() -> Self {
        Self {
            kind: BlushKind::Soft,
            colour: rgb(0xf0, 0x8c, 0x84),
            radius: [0.068, 0.028],
            strength: 0.95,
            wash: 0.3,
            base: 0.0,
            contour: 0.0,
            contour_colour: rgb(0xc8, 0x8e, 0x80),
        }
    }
}

impl Validate for CheekStyle {
    fn check(&self, c: &mut Check) {
        c.range("radius/0", self.radius[0], 0.005, 0.15);
        c.range("radius/1", self.radius[1], 0.005, 0.1);
        c.range("strength", self.strength, 0.0, 1.0);
        c.range("wash", self.wash, 0.0, 1.0);
        c.range("base", self.base, 0.0, 1.0);
        c.range("contour", self.contour, 0.0, 1.0);
    }
}

/// Kinds of mark (CB-23).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkKind {
    /// A scatter of small dots (`count` of them, placed from `seed`).
    Freckles,
    /// One small dot.
    BeautyMark,
    /// A painted shape: a stripe or a star on the cheek.
    Paint,
    /// A line scar.
    Scar,
}

/// One freckle patch, beauty mark, face paint or scar (CB-23), drawn on the `Base` layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FaceMark {
    /// What it is.
    pub kind: MarkKind,
    /// Centre, in head-local face-plane metres.
    pub at: [f64; 2],
    /// Size (patch radius, dot radius, stroke length), in metres.
    pub size: f64,
    /// Colour.
    pub colour: ColourRef,
    /// Opacity.
    pub opacity: f64,
    /// How many dots, for freckles.
    pub count: u8,
    /// Seed for placing freckles, so the pattern is stable.
    pub seed: u32,
}

impl Default for FaceMark {
    fn default() -> Self {
        Self {
            kind: MarkKind::BeautyMark,
            at: [0.0, 0.0],
            size: 0.003,
            colour: rgb(0x7a, 0x4a, 0x40),
            opacity: 0.8,
            count: 12,
            seed: 1,
        }
    }
}

impl Validate for FaceMark {
    fn check(&self, c: &mut Check) {
        c.range("at/0", self.at[0], -0.3, 0.3);
        c.range("at/1", self.at[1], -0.4, 0.3);
        c.range("size", self.size, 0.0003, 0.1);
        c.range("opacity", self.opacity, 0.0, 1.0);
    }
}

// ------------------------------------------------------------------------------------------------ effects

/// Which side of the face a one-sided mark goes on: the character's own.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkSide {
    /// The character's left (+x, the viewer's right).
    #[default]
    Left,
    /// The character's right.
    Right,
}

impl MarkSide {
    /// `+1` for the left, `-1` for the right: the sign of x on that side.
    pub fn sign(self) -> f32 {
        match self {
            Self::Left => 1.0,
            Self::Right => -1.0,
        }
    }
}

/// The anger vein (the cross-shaped popping mark at the temple), drawn in front of the hair when the `angerVein`
/// channel is on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AngerVeinStyle {
    /// The side of the head it pops on.
    pub side: MarkSide,
    /// Its half-size at full strength, in metres.
    pub size: f64,
    /// How far out from the eye's outer corner its centre sits, in metres.
    pub out: f64,
    /// How far above the brow line its centre sits, in metres.
    pub up: f64,
    /// The fill.
    pub colour: ColourRef,
    /// The dark outline round each stroke.
    pub outline: ColourRef,
}

impl Default for AngerVeinStyle {
    fn default() -> Self {
        Self {
            side: MarkSide::Left,
            size: 0.02,
            out: 0.0,
            up: 0.035,
            colour: rgb(0xe8, 0x2c, 0x34),
            outline: rgb(0x4a, 0x0c, 0x14),
        }
    }
}

impl Validate for AngerVeinStyle {
    fn check(&self, c: &mut Check) {
        c.range("size", self.size, 0.005, 0.05);
        c.range("out", self.out, -0.05, 0.08);
        c.range("up", self.up, -0.05, 0.12);
    }
}

/// The anime marks' own styles: what a mark looks like on this character, while channels say when it shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EffectStyle {
    /// The anger vein.
    pub anger_vein: AngerVeinStyle,
}

impl Validate for EffectStyle {
    fn check(&self, c: &mut Check) {
        c.nested("anger_vein", &self.anger_vein);
    }
}

// ------------------------------------------------------------------------------------------------ the profile

/// One character's face: layout, styles, resting face and expression library (`face.toml`, spec §7).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FaceProfile {
    /// Where the features sit.
    pub layout: FaceLayout,
    /// The eyes.
    pub eyes: EyeStyle,
    /// The brows.
    pub brows: BrowStyle,
    /// The mouth.
    pub mouth: MouthStyle,
    /// The nose.
    pub nose: NoseStyle,
    /// Blush.
    pub cheeks: CheekStyle,
    /// Freckles, beauty marks, face paint, scars.
    pub marks: Vec<FaceMark>,
    /// How the anime marks look on this character (the anger vein).
    pub effects: EffectStyle,
    /// How this face reads the channels: its resting face in paint terms (a sleepy half-lid, a slightly downturned
    /// mouth, a usual blush) and the coefficients of every formula (FE-06). Variants override single numbers: a
    /// performing face is livelier than a game face.
    pub mapping: FaceMapping,
    /// Channel values the character *starts from* before any driver writes (a standing "smug" brow, say), for the
    /// `rest` layer of the bus. Most characters leave it empty and put their resting look in `mapping.rest`.
    pub rest: ChannelValues,
    /// Named expressions and visemes.
    pub expressions: ExpressionSet,
    /// The expression the face returns to when no held expression shows (FE-03), for example a faint smile; left out,
    /// it returns to the resting face. It must name one of `expressions` that is a mood or an accent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_expression: Option<ExpressionId>,
    /// Which anime grammar rules run for this character and how strongly (FE-05). A rule with no entry runs at its
    /// default, so a file without this section behaves as before. Variants switch single rules by path.
    pub grammar: GrammarRules,
    /// The photoreal 3D eye and the skin round it, for a character whose eyes are modelled rather than painted. Left
    /// out, the face has painted eyes only; `face.set_style` on any `face/eye3d/...` field adds it with its defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eye3d: Option<EyeDesign>,
}

impl FaceProfile {
    /// Whether this character has fangs: its fang style is not [`FangKind::None`].
    pub fn has_fangs(&self) -> bool {
        self.mouth.fang.kind != FangKind::None
    }

    /// The `rest` layer of the bus: neutral with this character's `rest` values written.
    pub fn rest_buf(&self) -> ChannelBuf {
        let mut buf = ChannelBuf::NEUTRAL;
        self.rest.write_into(&mut buf);
        buf
    }
}

impl Validate for FaceProfile {
    fn check(&self, c: &mut Check) {
        c.nested("layout", &self.layout);
        c.nested("eyes", &self.eyes);
        c.nested("brows", &self.brows);
        c.nested("mouth", &self.mouth);
        c.nested("nose", &self.nose);
        c.nested("cheeks", &self.cheeks);
        for (i, m) in self.marks.iter().enumerate() {
            c.nested(&format!("marks/{i}"), m);
        }
        c.nested("effects", &self.effects);
        c.nested("mapping", &self.mapping);
        c.nested("grammar", &self.grammar);
        if let Some(eye) = &self.eye3d {
            c.nested("eye3d", eye);
        }
        if let Err(message) = self.rest.validate_ranges() {
            c.fail("rest", message);
        }
        for (id, e) in &self.expressions.0 {
            if e.deltas.0.values().any(|v| !v.is_finite()) {
                c.fail(&format!("expressions/{id}/deltas"), "a delta is not a finite number");
            }
            c.nested(&format!("expressions/{id}/playback"), &e.playback);
        }
        if let Some(id) = &self.default_expression {
            let ok = self
                .expressions
                .0
                .get(id)
                .is_some_and(|e| matches!(e.kind, ExpressionKind::Emotion | ExpressionKind::Accent));
            c.rule(&["default_expression"], ok, "the default expression must be one of the face's moods or accents");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_anim_api::Channel;

    #[test]
    fn the_default_profile_is_valid() {
        FaceProfile::default().validate().unwrap();
    }

    #[test]
    fn an_empty_document_is_a_complete_profile_and_typos_are_rejected() {
        let p: FaceProfile = serde_json::from_str("{}").unwrap();
        assert_eq!(p, FaceProfile::default());
        let e = serde_json::from_str::<FaceProfile>(r#"{"eyes":{"iris":{"widht":0.5}}}"#).unwrap_err().to_string();
        assert!(e.contains("widht"), "{e}");
    }

    #[test]
    fn partial_files_fill_in_defaults_and_round_trip() {
        let p: FaceProfile = serde_json::from_str(
            r##"{"eyes":{"iris":{"width":0.7,"top":"#112233"}},"mapping":{"rest":{"eye_open":1.3}},"rest":{"mouthSmileLeft":0.2}}"##,
        )
        .unwrap();
        assert_eq!(p.eyes.iris.width, 0.7);
        assert_eq!(p.eyes.iris.height, Iris::default().height);
        assert_eq!(p.mapping.rest_params().left_eye.open, 1.3);
        assert_eq!(p.mapping.mouth, crate::mapping::MouthMap::default());
        assert_eq!(p.rest_buf().get(Channel::MOUTH_SMILE_LEFT), 0.2);
        let back: FaceProfile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn validation_names_every_bad_field_by_path() {
        let mut p = FaceProfile::default();
        p.eyes.iris.width = 3.0;
        p.layout.mouth_y = 0.1;
        p.marks.push(FaceMark { opacity: 2.0, ..FaceMark::default() });
        p.mapping.eyes.shock_pupil = 2.0;
        p.rest.0.insert(Channel::JAW_OPEN, 9.0);
        let e = p.validate().unwrap_err();
        let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, ["layout/mouth_y", "eyes/iris/width", "marks/0/opacity", "mapping/eyes/shock_pupil", "rest"]);
        assert!(e.0[1].message.contains("0.2 to 1"), "{}", e.0[1].message);
    }

    #[test]
    fn ratios_describe_the_layout_independent_of_head_size() {
        let l = FaceLayout::default();
        let r = l.ratios(-0.25).unwrap();
        assert!((r.nose - 0.156).abs() < 0.01 && r.mouth > r.nose, "{r:?}");
        assert!(l.ratios(0.0).is_none(), "a chin above the eye line has no ratios");
        let a = FaceLayout::default();
        let b = FaceLayout { mouth_y: -0.2, ..FaceLayout::default() };
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
        assert!((a.lerp(&b, 0.5).mouth_y - -0.175).abs() < 1e-6);
    }

    #[test]
    fn a_profile_can_say_it_has_no_fangs() {
        assert!(FaceProfile::default().has_fangs(), "fangs stay the default, so existing profiles don't change");
        let p: FaceProfile = serde_json::from_str(r#"{"mouth":{"fang":{"kind":"none"}}}"#).unwrap();
        assert!(!p.has_fangs());
        assert_eq!(p.mouth.fang.width, FangStyle::default().width, "the rest of the style keeps its defaults");
        p.validate().unwrap();
        let back: FaceProfile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn old_pointed_fangs_load_as_both_and_single_fangs_have_one_corner() {
        let p: FaceProfile = serde_json::from_str(r#"{"mouth":{"fang":{"kind":"pointed"}}}"#).unwrap();
        assert_eq!(p.mouth.fang.kind, FangKind::Both);
        assert_eq!(serde_json::to_value(FangKind::Both).unwrap(), "both");
        for (text, kind) in
            [("one_left", FangKind::OneLeft), ("one-left", FangKind::OneLeft), ("one-right", FangKind::OneRight)]
        {
            let p: FaceProfile =
                serde_json::from_str(&format!(r#"{{"mouth":{{"fang":{{"kind":"{text}"}}}}}}"#)).unwrap();
            assert_eq!(p.mouth.fang.kind, kind, "{text}");
            assert!(p.has_fangs());
        }
        assert_eq!((FangKind::OneLeft.corners(), FangKind::OneRight.corners()), (&[1.0][..], &[-1.0][..]));
        assert_eq!((FangKind::Both.corners().len(), FangKind::None.corners().len()), (2, 0));
    }

    #[test]
    fn the_right_iris_takes_what_it_says_and_the_left_irises_the_rest() {
        let same = EyeStyle::default();
        assert_eq!(same.iris_paint(true), same.iris_paint(false), "no right iris: both eyes match");
        let p: FaceProfile =
            serde_json::from_str(r##"{"eyes":{"right_iris":{"mid":"#c03a3a","pupil":{"shape":"slit"}}}}"##).unwrap();
        p.validate().unwrap();
        let (left, right) = (p.eyes.iris_paint(false), p.eyes.iris_paint(true));
        assert_eq!(*right.mid, ColourRef::rgb(0xc0, 0x3a, 0x3a));
        assert_eq!((right.top, right.low, right.ring), (left.top, left.low, left.ring), "the rest is shared");
        assert_eq!((left.pupil.shape, right.pupil.shape), (PupilShape::Round, PupilShape::Slit));
        assert_eq!(*left.mid, Iris::default().mid, "the left eye is untouched");
        let back: FaceProfile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        assert!(!serde_json::to_string(&FaceProfile::default()).unwrap().contains("right_iris"), "old files unchanged");
    }

    #[test]
    fn the_anger_vein_has_a_side_and_a_checked_size() {
        let p: FaceProfile =
            serde_json::from_str(r#"{"effects":{"anger_vein":{"side":"right","size":0.03}}}"#).unwrap();
        assert_eq!((p.effects.anger_vein.side, p.effects.anger_vein.side.sign()), (MarkSide::Right, -1.0));
        p.validate().unwrap();
        let mut bad = p;
        bad.effects.anger_vein.size = 1.0;
        assert_eq!(bad.validate().unwrap_err().0[0].path, "effects/anger_vein/size");
    }

    #[test]
    fn eye_width_to_iod_is_half_an_eye_width_over_the_eye_distance() {
        // A half-width of 0.05 m on eyes 0.2 m apart: 0.25, not the 0.5 a full width would give.
        let l = FaceLayout { eye_width: 0.05, eye_spacing: 0.1, ..FaceLayout::default() };
        let r = l.ratios(-0.25).unwrap();
        assert!((r.eye_width_to_iod - 0.25).abs() < 1e-12, "{r:?}");
    }
}

#[cfg(test)]
mod shot_tests {
    use super::*;

    #[test]
    fn the_mouth_grows_in_a_wide_shot_and_eases_back_by_a_bust_shot() {
        let mouth = MouthStyle::default();
        let wide = crate::narrow(mouth.wide_shot_scale);
        assert!((mouth.shot_scale(Some(0.08)) - wide).abs() < 1e-6);
        assert!((mouth.shot_scale(Some(WIDE_SHOT)) - wide).abs() < 1e-6);
        assert!((mouth.shot_scale(Some(CLOSE_SHOT)) - 1.0).abs() < 1e-6);
        assert!((mouth.shot_scale(Some(0.8)) - 1.0).abs() < 1e-6);
        for unknown in [None, Some(0.0), Some(f32::NAN), Some(-1.0)] {
            assert!((mouth.shot_scale(unknown) - 1.0).abs() < 1e-6, "{unknown:?}");
        }
        // Monotonic and without a jump across the band.
        let mut last = f32::INFINITY;
        for step in 0..=100_u8 {
            let coverage = 0.05 + 0.3 * f32::from(step) / 100.0;
            let scale = mouth.shot_scale(Some(coverage));
            assert!(scale <= last && last - scale < 0.02 || last.is_infinite(), "{coverage}: {scale} after {last}");
            last = scale;
        }
        let still = MouthStyle { wide_shot_scale: 1.0, ..MouthStyle::default() };
        assert!((still.shot_scale(Some(0.05)) - 1.0).abs() < 1e-6);
    }
}
