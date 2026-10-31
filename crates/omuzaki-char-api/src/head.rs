//! The head part's parameters (spec CB-10, CB-11, CB-14): cranium, face-shape outline, ears and neck.
//!
//! **Head-local frame:** the origin is the head centre ([`Cranium::centre`] in bind space); axes are bind space (+Y up,
//! +Z forward, +X the character's left). Every height and position below, and the face layout in
//! `omuzaki_face_api::FaceLayout`, is in this frame, so moving the head or switching body templates moves them all.

use omuzaki_face_api::{Check, ColourRef, FaceProjection, FaceRect, Validate, narrow};
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::ProfileId;

/// The skin colours, by palette slot so head, body and ears always agree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SkinColours {
    /// The lit skin colour.
    pub base: ColourRef,
    /// The colour on the shadowed side (a warm tint, never grey).
    pub shade: ColourRef,
    /// The outline colour on skin.
    pub line: ColourRef,
    /// The rim-light colour.
    pub rim: ColourRef,
}

impl Default for SkinColours {
    fn default() -> Self {
        let slot = |name: &str, fallback: ColourRef| ColourRef::slot_or(name, fallback);
        Self {
            base: slot("skin", ColourRef::rgb(0xf9, 0xd3, 0xc5)),
            shade: slot("skin_shade", ColourRef::rgb(0xf2, 0xb4, 0xab)),
            line: slot("skin_line", ColourRef::rgb(0xc9, 0x8a, 0x80)),
            rim: ColourRef::rgb(0xff, 0xd9, 0xcf),
        }
    }
}

/// The cranium: an ellipsoid with a fuller lower face, before the face-shape outline is applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Cranium {
    /// Head centre in bind space: the origin of the head-local frame. `None` (the default) seats the head on the
    /// template it is built on: [`Cranium::DEFAULT_CENTRE`] raised by as much as the template's head joint stands above
    /// soft-sd's, so the head meets the neck on every template and rides it when the body's sliders move it (#825). A
    /// centre written here is taken as written, on every template (#960). [`Cranium::centre`] reads it with soft-sd's
    /// seat for `None`, where no template is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub centre: Option<[f64; 3]>,
    /// Half-width (x).
    pub half_width: f64,
    /// Half-depth at the front (+z).
    pub half_depth_front: f64,
    /// Half-depth at the back (−z).
    pub half_depth_back: f64,
    /// Half-height above the centre.
    pub half_height_up: f64,
    /// Half-height below the centre at the back (into the neck).
    pub half_height_down_back: f64,
    /// Half-height below the centre at the front (the chin side): longer than at the back.
    pub half_height_down_front: f64,
    /// How much fuller the lower front cheeks are, in metres.
    pub cheek_fullness: f64,
    /// How much the face plane is flattened for the painted features, `0..=0.2`.
    pub face_flatten: f64,
}

impl Default for Cranium {
    fn default() -> Self {
        Self {
            centre: None,
            half_width: 0.186,
            half_depth_front: 0.184,
            half_depth_back: 0.181,
            half_height_up: 0.186,
            half_height_down_back: 0.15,
            half_height_down_front: 0.197,
            cheek_fullness: 0.01,
            face_flatten: 0.035,
        }
    }
}

impl Cranium {
    /// Where an unset centre puts the head on soft-sd, the template the default cranium is sculpted on.
    pub const DEFAULT_CENTRE: [f64; 3] = [0.0, 0.78, -0.02];

    /// The centre as written, or soft-sd's seat ([`Cranium::DEFAULT_CENTRE`]) when it is unset: what a head built with
    /// no template under it uses.
    #[must_use]
    pub fn centre(&self) -> [f64; 3] {
        self.raised_centre(0.0)
    }

    /// The centre as written, or, when it is unset, [`Cranium::DEFAULT_CENTRE`] raised by `rise`: how far the head
    /// joint of the template the head is built on stands above soft-sd's.
    #[must_use]
    pub fn raised_centre(&self, rise: f64) -> [f64; 3] {
        let [x, y, z] = Self::DEFAULT_CENTRE;
        self.centre.unwrap_or([x, y + rise, z])
    }
}

impl Validate for Cranium {
    fn check(&self, c: &mut Check) {
        c.range("half_width", self.half_width, 0.05, 0.4);
        c.range("half_depth_front", self.half_depth_front, 0.05, 0.4);
        c.range("half_depth_back", self.half_depth_back, 0.05, 0.4);
        c.range("half_height_up", self.half_height_up, 0.05, 0.4);
        c.range("half_height_down_back", self.half_height_down_back, 0.05, 0.4);
        c.range("half_height_down_front", self.half_height_down_front, 0.05, 0.4);
        c.range("cheek_fullness", self.cheek_fullness, 0.0, 0.05);
        c.range("face_flatten", self.face_flatten, 0.0, 0.2);
        for (i, v) in self.centre.iter().flatten().enumerate() {
            c.range(&format!("centre/{i}"), *v, -4.0, 4.0);
        }
    }
}

/// A chin: the lower face is lengthened so the lowest point lands exactly here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Chin {
    /// Head-local height of the chin point (negative: below the centre).
    pub y: f64,
    /// Head-local height below which the lower face is stretched down.
    pub stretch_top: f64,
    /// Smallest allowed ratio between the target outline width and the cranium's width (stops pinching). The head
    /// applies it only near the chin's tip, held to `0.6..=0.75`.
    pub min_ratio: f64,
}

impl Default for Chin {
    fn default() -> Self {
        Self { y: -0.21, stretch_top: -0.08, min_ratio: 0.6 }
    }
}

/// The face-shape outline (CB-10): a superellipse in the front view that the head's lower face is shaped to.
/// Presets (`round-u`, `egg-oval`, `mochi`, `heart`, `long`) are library data that ops blend into these numbers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HeadOutline {
    /// Half-width of the outline at its top (the temples).
    pub half_width: f64,
    /// Head-local height where the taper toward the chin begins.
    pub top: f64,
    /// How far below `top` the outline closes, when there is no `chin`. With a chin the outline closes at the chin and
    /// this is unused.
    pub height: f64,
    /// Superellipse exponent: 2 is an ellipse; lower is pointier, higher squarer.
    pub exponent: f64,
    /// Superellipse exponent below the cheek line, joined C1 to `exponent` above it; `None` uses `exponent` for the
    /// whole outline (today's single exponent).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exponent_lower: Option<f64>,
    /// A longer chin (close-up faces), or `None` to close the outline at `top - height`: a chinless head is stretched
    /// (or pressed in slightly) so its lowest point lands there, so [`HeadOutline::chin_y`] holds for it too.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chin: Option<Chin>,
    /// How far the cheeks bulge forward either side of the mouth (chubby), in metres.
    pub cheek_puff: f64,
}

impl Default for HeadOutline {
    fn default() -> Self {
        Self {
            half_width: 0.15,
            top: -0.1,
            height: 0.1,
            exponent: 1.9,
            exponent_lower: None,
            chin: None,
            cheek_puff: 0.0,
        }
    }
}

impl HeadOutline {
    /// The head-local height of the chin: the stretched chin if there is one, else where the outline closes.
    pub fn chin_y(&self) -> f64 {
        self.chin.map_or(self.top - self.height, |c| c.y)
    }

    /// Linear blend (CB-10 "blendable"). If only one side has a chin, the chin switches at `t = 0.5`. `exponent_lower`
    /// is exact at the ends: `t = 0` gives `self`'s value and `t = 1` gives `other`'s, `None` included. Between them a
    /// `None` side counts as that side's `exponent`, so the blend stays continuous; two `None`s stay `None`. It blends
    /// its reciprocal linearly: near the chin a superellipse's width grows as the `1 / exponent` power of the height,
    /// so the chin then moves by about as much at every step of a blend, where a linear blend toward a low exponent
    /// would leave most of the change to its last steps.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        let m = |a: f64, b: f64| a + (b - a) * t;
        let exponent_lower = match (self.exponent_lower, other.exponent_lower) {
            (None, None) => None,
            _ if t <= 0.0 => self.exponent_lower,
            _ if t >= 1.0 => other.exponent_lower,
            (a, b) => Some(1.0 / m(1.0 / a.unwrap_or(self.exponent), 1.0 / b.unwrap_or(other.exponent))),
        };
        let chin = match (self.chin, other.chin) {
            (Some(a), Some(b)) => Some(Chin {
                y: m(a.y, b.y),
                stretch_top: m(a.stretch_top, b.stretch_top),
                min_ratio: m(a.min_ratio, b.min_ratio),
            }),
            (a, b) => {
                if t < 0.5 {
                    a
                } else {
                    b
                }
            }
        };
        Self {
            half_width: m(self.half_width, other.half_width),
            top: m(self.top, other.top),
            height: m(self.height, other.height),
            exponent: m(self.exponent, other.exponent),
            exponent_lower,
            chin,
            cheek_puff: m(self.cheek_puff, other.cheek_puff),
        }
    }
}

impl Validate for HeadOutline {
    fn check(&self, c: &mut Check) {
        c.range("half_width", self.half_width, 0.05, 0.4);
        c.range("top", self.top, -0.3, 0.2);
        c.range("height", self.height, 0.02, 0.4);
        c.range("exponent", self.exponent, 1.0, 4.0);
        if let Some(lower) = self.exponent_lower {
            c.range("exponent_lower", lower, 1.0, 4.0);
        }
        c.range("cheek_puff", self.cheek_puff, 0.0, 0.05);
        if let Some(chin) = &self.chin {
            c.range("chin/y", chin.y, -0.5, 0.0);
            c.range("chin/stretch_top", chin.stretch_top, -0.4, 0.2);
            c.range("chin/min_ratio", chin.min_ratio, 0.2, 1.0);
            if chin.stretch_top <= chin.y {
                c.fail("chin/stretch_top", "must be above the chin point");
            }
        }
    }
}

/// A radius map traced from a reference mesh or the sheet silhouettes (CB-11): the distance from the head centre in
/// a grid of polar directions, plus a per-cell weight saying where the trace had data. Too large for a project text
/// file (the prototype's is ~100 KB), so it is stored as an asset: little-endian `f64`s, all radii first (row by row,
/// `theta` from the top, then `phi` from the front), then all weights.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TracedHead {
    /// The asset's content hash (BLAKE3), resolved through the project's asset store (`omuzaki-store-api`).
    pub asset: ContentHash,
    /// Rows: steps of `theta` from the top to the chin.
    pub theta_steps: u16,
    /// Columns: steps of `phi` once around.
    pub phi_steps: u16,
    /// How much of the trace replaces the analytic cranium where it has data, `0..=1`.
    pub mix: f64,
}

/// Ear types (CB-14).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EarKind {
    /// No ears (hidden by hair, or a hat).
    None,
    /// Small rounded human ears.
    Human,
    /// Long pointed ears.
    Elf,
    /// Animal ears on top of the head (cat, fox).
    Animal,
}

/// Ears.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Ears {
    /// Which kind.
    pub kind: EarKind,
    /// Size multiplier.
    pub size: f64,
    /// Position of the left ear, head-local (the right is mirrored).
    pub position: [f64; 3],
    /// How far the ear flares out from the head, degrees.
    pub flare_deg: f64,
    /// How far it tilts back, degrees.
    pub tilt_deg: f64,
    /// How long an [`EarKind::Elf`] ear's pointed tip is, metres (0.04 to 0.14); `None` for the length its `size` gives
    /// (0.09 m per unit, held to that range), which is what every ear saved before this field had. Ignored by the
    /// other kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(extend("x-ui" = {"level": "advanced"}))]
    pub tip_length: Option<f64>,
    /// How far an [`EarKind::Elf`] ear's tip leans back from straight up, degrees (0 to 45), in the plane of the
    /// shell. Ignored by the other kinds.
    #[serde(skip_serializing_if = "Ears::is_default_tip_angle")]
    #[schemars(extend("x-ui" = {"level": "advanced"}))]
    pub tip_angle_deg: f64,
    /// What an [`EarKind::Animal`] ear is: the animal, where it stands and its colours. Ignored by the other kinds, so
    /// a form shows it folded.
    #[serde(skip_serializing_if = "AnimalEars::is_default")]
    #[schemars(extend("x-ui" = {"level": "advanced"}))]
    pub animal: AnimalEars,
}

impl Default for Ears {
    fn default() -> Self {
        Self {
            kind: EarKind::Human,
            size: 1.0,
            position: [0.184, -0.066, -0.012],
            flare_deg: 42.0,
            tilt_deg: 7.0,
            tip_length: None,
            tip_angle_deg: Self::TIP_ANGLE_DEG,
            animal: AnimalEars::default(),
        }
    }
}

impl Ears {
    /// The default lean of an elf ear's tip, degrees.
    pub const TIP_ANGLE_DEG: f64 = 15.0;

    /// Whether `deg` is the default tip lean, which a file leaves out.
    #[allow(clippy::trivially_copy_pass_by_ref, clippy::float_cmp)] // serde passes a reference; only the exact default is left out
    fn is_default_tip_angle(deg: &f64) -> bool {
        *deg == Self::TIP_ANGLE_DEG
    }
}

/// The animal an [`EarKind::Animal`] ear is drawn from (CB-14).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EarSpecies {
    /// Pointed and upright, a little wider than tall.
    #[default]
    Cat,
    /// Tall and pointed.
    Fox,
    /// Long, narrow and rounded at the top; it sways on a short spring chain.
    Rabbit,
    /// Broad at the base, pointed.
    Wolf,
}

impl EarSpecies {
    /// Every species, in declaration order.
    pub const ALL: [Self; 4] = [Self::Cat, Self::Fox, Self::Rabbit, Self::Wolf];
}

/// Animal ears on top of the head: which animal, where they stand, and their colours. `Ears::size` scales them, and
/// `Ears::flare_deg` and `Ears::tilt_deg` turn them from the species' own stance by their difference from the defaults
/// (42 and 7 degrees), so the defaults give each species its natural stance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AnimalEars {
    /// The animal.
    pub species: EarSpecies,
    /// How far down from the top of the head the left ear stands, degrees; `None` for the species' own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_top_deg: Option<f64>,
    /// How far round from the front toward the left the left ear stands, degrees (the right mirrors it); `None` for
    /// the species' own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_deg: Option<f64>,
    /// The fur outside: the hair's mid colour by default, so the ears match the hair.
    pub fur: ColourRef,
    /// The inner panel.
    pub inner: ColourRef,
}

impl Default for AnimalEars {
    fn default() -> Self {
        Self {
            species: EarSpecies::Cat,
            from_top_deg: None,
            round_deg: None,
            fur: ColourRef::slot_or("hair_mid", ColourRef::rgb(0x8a, 0x5a, 0x44)),
            inner: ColourRef::slot_or("ear_inner", ColourRef::rgb(0xf0, 0xa8, 0xa8)),
        }
    }
}

impl AnimalEars {
    /// Whether these are the defaults (a file leaves them out).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The neck.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Neck {
    /// Radius under the chin.
    pub radius_top: f64,
    /// Radius at the collar.
    pub radius_bottom: f64,
    /// Length.
    pub height: f64,
    /// How far the head sits lower on the neck, so a chibi chin rests near the collar (negative raises it for close-ups).
    pub drop: f64,
}

impl Default for Neck {
    fn default() -> Self {
        Self { radius_top: 0.03, radius_bottom: 0.038, height: 0.11, drop: 0.016 }
    }
}

/// The face's bony and fleshy forms sculpted into the head (#2697): each a height in metres on the head as authored
/// (the head part's frame, 0.38 m from chin to crown), out of the cranium (positive) or into it (negative where noted),
/// placed from the painted face's layout so the forms sit under the drawn features. All zero (the default) is the kit's
/// smooth anime cranium; the realistic styles raise them, so a lit head turns like a skull with a face rather than a
/// ball with one painted on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Sculpt {
    /// The brow ridge over both eyes and the bridge between them (the glabella), out, `0..=0.03`.
    pub brow: f64,
    /// The eye sockets under the brow, in, `0..=0.03`: the lids and eyes sit back in the skull.
    pub sockets: f64,
    /// The cheekbones below and outside the eyes, out, `0..=0.03`.
    pub cheekbones: f64,
    /// The hollow of each temple beside the brow, in, `0..=0.02`.
    pub temples: f64,
    /// The nose: its bridge rising from between the eyes to its tip over the painted nose, its wings and the drop under
    /// it, out, `0..=0.06`.
    pub nose: f64,
    /// The muzzle round the mouth (the teeth's curve under the lips), with the lips' fullness and the furrow from the
    /// nose's wings to the mouth's corners, out, `0..=0.03`.
    pub mouth: f64,
    /// The chin's front, out, `0..=0.03`.
    pub chin: f64,
    /// The angle of the jaw below the ears, out, `0..=0.03`: a squarer, more defined lower face.
    pub jaw: f64,
}

impl Sculpt {
    /// True when every form is zero: the smooth cranium, which a saved character leaves out.
    pub fn is_flat(&self) -> bool {
        *self == Self::default()
    }
}

impl Validate for Sculpt {
    fn check(&self, c: &mut Check) {
        c.range("brow", self.brow, 0.0, 0.03);
        c.range("sockets", self.sockets, 0.0, 0.03);
        c.range("cheekbones", self.cheekbones, 0.0, 0.03);
        c.range("temples", self.temples, 0.0, 0.02);
        c.range("nose", self.nose, 0.0, 0.06);
        c.range("mouth", self.mouth, 0.0, 0.03);
        c.range("chin", self.chin, 0.0, 0.03);
        c.range("jaw", self.jaw, 0.0, 0.03);
    }
}

/// How the painted face window sits on the head: the rectangle the face layers cover.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FaceWindow {
    /// Half the window's width.
    pub half_width: f64,
    /// Head-local height of its top edge.
    pub top: f64,
    /// How far below the chin it reaches.
    pub below_chin: f64,
    /// Head-local `z` where the painted face starts to fade in, and where it is fully visible.
    pub front_fade: [f64; 2],
}

impl Default for FaceWindow {
    fn default() -> Self {
        Self { half_width: 0.215, top: 0.105, below_chin: 0.025, front_fade: [0.0, 0.07] }
    }
}

/// The parameters of the `head` part kind.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HeadParams {
    /// The cranium.
    pub cranium: Cranium,
    /// The face-shape outline.
    pub outline: HeadOutline,
    /// The face's forms sculpted into the head: brow, sockets, cheekbones, nose, mouth, chin and jaw. All zero, the
    /// default, is the smooth cranium.
    #[serde(skip_serializing_if = "Sculpt::is_flat")]
    pub sculpt: Sculpt,
    /// The face window.
    pub window: FaceWindow,
    /// An optional traced radius map.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub traced: Option<TracedHead>,
    /// Ears.
    pub ears: Ears,
    /// The neck.
    pub neck: Neck,
    /// Skin colours.
    pub skin: SkinColours,
    /// The preset the outline was last derived from; informational (the numbers above are what builds).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preset: Option<ProfileId>,
}

impl HeadParams {
    /// Where the painted face layers land, head-local, for the outline as written. The face painter's layers cover
    /// exactly this rectangle. Follows the chin, so a variant with a longer chin gets a taller window. The head part
    /// builds from the outline clamped into its plausible space and publishes the projection of that outline, so the
    /// window and the mesh agree for any slider values; this method, on parameters outside that space, gives the
    /// window of the unclamped request.
    pub fn face_projection(&self) -> FaceProjection {
        let w = &self.window;
        FaceProjection {
            rect: FaceRect {
                x0: narrow(-w.half_width),
                x1: narrow(w.half_width),
                y0: narrow(self.outline.chin_y() - w.below_chin),
                y1: narrow(w.top),
            },
            front_fade: w.front_fade.map(narrow),
        }
    }
}

impl Validate for HeadParams {
    fn check(&self, c: &mut Check) {
        c.nested("cranium", &self.cranium);
        c.nested("outline", &self.outline);
        c.nested("sculpt", &self.sculpt);
        c.range("window/half_width", self.window.half_width, 0.05, 0.5);
        c.range("window/top", self.window.top, -0.2, 0.4);
        c.range("window/below_chin", self.window.below_chin, 0.0, 0.1);
        if let Some(t) = &self.traced {
            c.range("traced/mix", t.mix, 0.0, 1.0);
            if t.theta_steps < 4 || t.phi_steps < 8 {
                c.fail("traced", "the grid needs at least 4 theta steps and 8 phi steps");
            }
        }
        c.range("ears/size", self.ears.size, 0.2, 3.0);
        if let Some(length) = self.ears.tip_length {
            c.range("ears/tip_length", length, 0.04, 0.14);
        }
        c.range("ears/tip_angle_deg", self.ears.tip_angle_deg, 0.0, 45.0);
        if let Some(deg) = self.ears.animal.from_top_deg {
            c.range("ears/animal/from_top_deg", deg, 0.0, 80.0);
        }
        if let Some(deg) = self.ears.animal.round_deg {
            c.range("ears/animal/round_deg", deg, 0.0, 120.0);
        }
        c.range("neck/height", self.neck.height, 0.0, 0.5);
        c.range("neck/radius_top", self.neck.radius_top, 0.005, 0.2);
        c.range("neck/radius_bottom", self.neck.radius_bottom, 0.005, 0.2);
        c.range("neck/drop", self.neck.drop, -0.1, 0.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cranium_may_sit_as_high_as_the_tallest_template_puts_it() {
        // The 7.5 head template's head centre is 2.7 m up in the shared head frame.
        let mut c = Cranium { centre: Some([0.0, 2.7, -0.02]), ..Cranium::default() };
        c.validate().unwrap();
        c.centre = Some([0.0, 4.5, -0.02]);
        assert!(c.validate().is_err());
    }

    #[test]
    fn an_unset_centre_is_soft_sds_seat_raised_and_a_written_one_is_kept() {
        let unset = Cranium::default();
        assert_eq!(unset.centre(), Cranium::DEFAULT_CENTRE);
        assert_eq!(unset.raised_centre(0.5), [0.0, 0.78 + 0.5, -0.02]);
        // A written centre equal to the old default is the author's, not a stand-in for "unset".
        let written = Cranium { centre: Some(Cranium::DEFAULT_CENTRE), ..Cranium::default() };
        assert_eq!(written.raised_centre(0.5), Cranium::DEFAULT_CENTRE);
        // Unset is not written, and a file without it reads as unset.
        let json = serde_json::to_value(&unset).unwrap();
        assert!(json.get("centre").is_none(), "{json}");
        let read: Cranium = serde_json::from_value(serde_json::json!({ "centre": [0.0, 0.78, -0.02] })).unwrap();
        assert_eq!(read, written);
    }

    #[test]
    fn the_default_head_is_valid_and_round_trips() {
        let h = HeadParams::default();
        h.validate().unwrap();
        let back: HeadParams = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
        assert_eq!(back, h);
        assert!(serde_json::from_str::<HeadParams>(r#"{"cranium":{"half_widht":1}}"#).is_err());
    }

    #[test]
    fn parameters_are_f64_so_files_and_agents_see_the_numbers_they_wrote() {
        let json = serde_json::to_string(&HeadParams::default()).unwrap();
        assert!(json.contains(r#""half_width":0.186,"#), "no f32 widening noise like 0.18600000441: {json}");
        let p = HeadParams { cranium: Cranium { half_width: 0.6, ..Cranium::default() }, ..HeadParams::default() };
        assert!(serde_json::to_string(&p).unwrap().contains(r#""half_width":0.6,"#));
    }

    #[test]
    fn the_face_window_follows_the_chin() {
        let mut h = HeadParams::default();
        let short = h.face_projection().rect;
        h.outline.chin = Some(Chin { y: -0.3, ..Chin::default() });
        let long = h.face_projection().rect;
        assert!(long.y0 < short.y0);
        assert_eq!(long.y1, short.y1);
        assert!((long.y0 - (-0.325)).abs() < 1e-6, "chin - below_chin: {}", long.y0);
        assert!((long.width() - 0.43).abs() < 1e-6);
    }

    #[test]
    fn outlines_blend_and_switch_chins_at_the_midpoint() {
        let a = HeadOutline { exponent: 2.0, ..HeadOutline::default() };
        let b = HeadOutline { exponent: 1.8, chin: Some(Chin::default()), ..HeadOutline::default() };
        assert_eq!(a.lerp(&b, 0.0), a);
        assert!(a.lerp(&b, 0.49).chin.is_none() && a.lerp(&b, 0.5).chin.is_some());
        assert!((a.lerp(&b, 0.5).exponent - 1.9).abs() < 1e-6);
        assert_eq!(b.lerp(&b, 0.3), b);
    }

    #[test]
    fn a_missing_lower_exponent_is_not_written_and_old_files_still_parse() {
        let json = serde_json::to_string(&HeadOutline::default()).unwrap();
        assert!(!json.contains("exponent_lower"), "{json}");
        let old: HeadOutline = serde_json::from_str(&json).unwrap();
        assert_eq!(old.exponent_lower, None);
        let lower: HeadOutline = serde_json::from_str(r#"{"exponent_lower":1.6}"#).unwrap();
        assert_eq!(lower.exponent_lower, Some(1.6));
        assert!(serde_json::to_string(&lower).unwrap().contains(r#""exponent_lower":1.6"#));
    }

    #[test]
    fn two_missing_lower_exponents_blend_to_none() {
        let a = HeadOutline { exponent: 2.0, ..HeadOutline::default() };
        let b = HeadOutline { exponent: 1.8, ..HeadOutline::default() };
        for t in [0.0, 0.3, 0.5, 1.0] {
            assert_eq!(a.lerp(&b, t).exponent_lower, None, "t = {t}");
        }
    }

    #[test]
    fn a_missing_lower_exponent_blends_as_its_own_exponent() {
        let some = HeadOutline { exponent: 2.0, exponent_lower: Some(1.5), ..HeadOutline::default() };
        let none = HeadOutline { exponent: 1.8, ..HeadOutline::default() };
        assert_eq!(some.lerp(&none, 0.0).exponent_lower, Some(1.5));
        // Half way is the harmonic mean: the reciprocals blend linearly.
        let half = 2.0 / (1.0 / 1.5 + 1.0 / 1.8);
        assert!((some.lerp(&none, 0.5).exponent_lower.unwrap() - half).abs() < 1e-6);
        assert_eq!(some.lerp(&none, 1.0).exponent_lower, None, "t = 1 is `other`, exactly");
        // The `None` side on the left counts as its own `exponent` (1.8 here), so the blend is symmetric.
        assert!((none.lerp(&some, 0.5).exponent_lower.unwrap() - half).abs() < 1e-6);
        assert_eq!(none.lerp(&some, 1.0).exponent_lower, Some(1.5));
    }

    #[test]
    fn a_mixed_lower_exponent_is_exact_at_the_ends() {
        let some = HeadOutline { exponent: 2.0, exponent_lower: Some(1.5), ..HeadOutline::default() };
        let none = HeadOutline { exponent: 1.8, ..HeadOutline::default() };
        // `t = 0` is `self` and `t = 1` is `other`, so no no-op blend gains or loses the key.
        assert_eq!(none.lerp(&some, 0.0).exponent_lower, None);
        assert_eq!(some.lerp(&none, 1.0).exponent_lower, None);
        assert_eq!(some.lerp(&none, 0.0).exponent_lower, Some(1.5));
        assert_eq!(none.lerp(&some, 1.0).exponent_lower, Some(1.5));
        assert_eq!(none.lerp(&some, 0.0), none);
    }

    #[test]
    fn a_lower_exponent_outside_the_range_names_its_field() {
        for bad in [0.5, 4.5, f64::NAN] {
            let mut h = HeadParams::default();
            h.outline.exponent_lower = Some(bad);
            let e = h.validate().unwrap_err();
            let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
            assert_eq!(paths, ["outline/exponent_lower"], "{bad}");
        }
        let mut h = HeadParams::default();
        h.outline.exponent_lower = Some(1.6);
        h.validate().unwrap();
    }

    #[test]
    fn validation_catches_a_chin_above_its_stretch_line() {
        let mut h = HeadParams::default();
        h.outline.chin = Some(Chin { y: -0.1, stretch_top: -0.2, min_ratio: 0.6 });
        h.traced = Some(TracedHead { asset: "a".repeat(64).parse().unwrap(), theta_steps: 2, phi_steps: 64, mix: 2.0 });
        let e = h.validate().unwrap_err();
        let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(paths, ["outline/chin/stretch_top", "traced/mix", "traced"]);
    }

    #[test]
    fn skin_defaults_use_palette_slots() {
        assert_eq!(SkinColours::default().base.to_string(), "@skin");
    }
}
