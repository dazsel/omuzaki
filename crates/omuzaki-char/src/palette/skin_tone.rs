//! The curated skin-tone range (CB-04): twelve tones from very light to very dark, each in a warm, a neutral
//! and a cool undertone, and the documented rules that turn one into the palette slots a toon-lit character needs.
//!
//! # Why derived, and how
//!
//! A flat table of 36 hand-picked colour sets would drift apart: one tone's shade a little muddier, another's blush a
//! little weaker. Instead each tone file (`library/skin-tones/tone-NN.toml`) holds only the **base** in OKLCH plus the
//! renderer's skin treatment for that tone, and [`skin_ramp`] derives the rest by rules that move smoothly with the
//! base lightness (`t`, 0 on the darkest tone to 1 on the lightest). OKLCH is the working space because lightness,
//! chroma and hue are separate dials there, so "keep the chroma, rotate the hue" means what it says.
//!
//! | Slot | Rule (all in OKLCH, from the base `b`) |
//! |---|---|
//! | `skin` | the base, with the undertone's hue turn and chroma factor |
//! | `skin_shade` | the colour of the shadow side: hue rotated 6 to 13 degrees towards warm red, lightness x 0.80 to 0.915, chroma *raised* a little so shadow never greys. A material gives the renderer [`SkinSlots::toon_shade`], not this colour: `Toon::shade` is a multiplier on the skin, and a dark colour used raw would multiply a deep tone to black |
//! | `skin_line` | 0.09 to 0.16 darker than the shade, chroma kept, never black (the outline colour) |
//! | `skin_rim` | lighter than the base (0.26 on the deepest tone, 0.05 on the lightest), chroma matched, hue pulled 30% of the way to the key light's |
//! | `blush` | a saturated warm red 0.12 lighter or darker than the base: darker (a pink) on light skin, lighter (a red-brown, hue 36 rather than 24) on deep skin, so it separates from the skin on every tone |
//! | `lip` | the same idea, deeper and a little redder |
//! | `ear_inner` | slightly darker than the base, chroma x 1.4 to 1.6, hue towards red |
//!
//! Every result is gamut-mapped by lowering chroma at constant lightness and hue, then rounded to 8 bits; the rules
//! start from the rounded base, so the relationships hold on the colours that actually ship (`tests/skin_tone.rs`
//! checks them on all 12 x 3 ramps).
//!
//! # `skin_floor` and `hue_keep`
//!
//! Each tone also carries the two numbers of the renderer's `Skin` treatment ([`SkinToneParams`]): the floor keeps a
//! shadow from sinking below a fraction of the albedo, and `hue_keep` pulls coloured light back to the skin hue, so a
//! blue moonlight on light skin does not turn it lavender-grey. It is a trade-off: too high and the warm shade tint
//! is pulled out of the shadows (they go beige and muddy), too low and a coloured key leaves a mauve haze. The tones
//! sit between 0.58 and 0.72: highest on the palest skin (least chroma to lose) and on the mid tones (the most
//! saturated, so a blue light neutralises them hardest), lower on deep skin. The floor rises towards the deep end
//! because the filmic tone curve crushes dark values hardest.
//!
//! The treatment follows the colour, not the tone id. Parts call [`SkinToneParams::for_colour`] on the resolved `skin`
//! slot, which interpolates the twelve tuned values by that colour's lightness, so a custom colour or a non-human skin
//! gets a treatment too. The tone id recorded on a character is informational by contract: a person can pick any skin
//! colour, and a treatment looked up by id alone would leave every such colour on the default.
//!
//! # References
//!
//! The range is the project's own palette work; nothing is copied from a third-party palette.
//! - The light end is anchored on the prototype's own skin (`#f9d3c5` base, `#f2b4ab` shade, `#c98a80` line in
//!   the earlier web prototype): shade hue 13 degrees towards red, chroma x 1.6, line 0.13 darker. The lighting
//!   presets and the `SKIN_FLOOR` / `SKIN_OUT` treatment it is judged under are the prototype's `js/viewer.js` rigs.
//! - The lightness span (0.895 to 0.39 in OKLCH) was checked against the ends of the Monk Skin Tone Scale (Ellis
//!   Monk and Google, CC BY 4.0, <https://skintone.google>), which spans 0.95 to 0.26 at photographic chroma; the
//!   range here is a little narrower and more saturated because it is for stylised, toon-lit characters. No values
//!   were taken from it.
//! - `OKLab` and its gamut idea: Björn Ottosson, "A perceptual color space for image processing" (2020), whose
//!   matrices are published under the MIT licence.
//!
//! # The ops
//!
//! `palette.set_skin_tone` writes the seven palette slots, `CharacterDef.skin_tone`, points the head and body `skin`
//! colours at `@skin`, `@skin_shade`, `@skin_line` and `@skin_rim`, and the face's `cheeks.colour` and
//! `mouth.lips.colour` at `@blush` and `@lip`, in one patch, so undo is exact. `palette.skin_tones` lists the range.

use std::sync::OnceLock;

use omuzaki_char_api::{CharId, CharacterDef, SkinColours, SkinToneId, paths};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_face_api::{ColourRef, PaletteSlot};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::Skin;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

// ------------------------------------------------------------------------------------------------ colour maths

/// A colour in OKLCH: perceptual lightness `l` (0 black to 1 white), chroma `c` and hue `h` in degrees (about 20 is
/// red, 55 orange-peach, 90 yellow). Used for tuning and for checking: the product still stores sRGB [`Colour`]s.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oklch {
    /// Lightness.
    pub l: f64,
    /// Chroma (colourfulness).
    pub c: f64,
    /// Hue in degrees.
    pub h: f64,
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// `OKLab` `[L, a, b]` of an sRGB colour (alpha ignored).
// The one-letter names are the published ones (cone responses l, m, s; axes L, a, b): matching them keeps the maths
// checkable against the source.
#[allow(clippy::many_single_char_names)]
fn oklab(colour: Colour) -> [f64; 3] {
    let [r, g, b] = [colour.r, colour.g, colour.b].map(|v| srgb_to_linear(f64::from(v) / 255.0));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// Slack when testing whether a linear channel is inside `0..=1`: absorbs the rounding of the matrices.
const GAMUT_EPS: f64 = 1e-4;

impl Oklch {
    /// The OKLCH of an sRGB colour.
    pub fn from_colour(colour: Colour) -> Self {
        let [l, a, b] = oklab(colour);
        Self { l, c: a.hypot(b), h: b.atan2(a).to_degrees().rem_euclid(360.0) }
    }

    /// Linear sRGB of this colour, unclamped (outside `0..=1` means out of gamut). Names as in [`oklab`].
    #[allow(clippy::many_single_char_names)]
    fn linear(self) -> [f64; 3] {
        let (sin, cos) = self.h.to_radians().sin_cos();
        let (a, b) = (self.c * cos, self.c * sin);
        let l = (self.l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
        let m = (self.l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
        let s = (self.l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
        [
            4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
            -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
            -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
        ]
    }

    fn in_gamut(self) -> bool {
        self.linear().iter().all(|v| (-GAMUT_EPS..=1.0 + GAMUT_EPS).contains(v))
    }

    /// The sRGB colour, rounded to 8 bits. A colour outside the sRGB gamut keeps its lightness and hue and loses
    /// chroma until it fits, so a saturated shade never turns into a different colour.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // each channel is clamped to 0..=255 first
    pub fn to_colour(self) -> Colour {
        let here = Self { l: self.l.clamp(0.0, 1.0), c: self.c.max(0.0), h: self.h };
        let fitted = if here.in_gamut() {
            here
        } else {
            let (mut inside, mut outside) = (0.0, here.c);
            for _ in 0..32 {
                let mid = f64::midpoint(inside, outside);
                if (Self { c: mid, ..here }).in_gamut() {
                    inside = mid;
                } else {
                    outside = mid;
                }
            }
            Self { c: inside, ..here }
        };
        let [r, g, b] = fitted.linear().map(|v| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
        Colour::rgb(r, g, b)
    }
}

/// The distance between two colours in `OKLab`, times 100 (so 1 is about the smallest visible step and 12 is a clear
/// difference).
pub fn delta_e(a: Colour, b: Colour) -> f64 {
    let (a, b) = (oklab(a), oklab(b));
    100.0 * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The signed turn in degrees, in `(-180, 180]`, that takes hue `from` to hue `to` the short way. Negative is towards
/// red (smaller hue angle), which is the "warm" direction for skin.
pub fn hue_delta(from: f64, to: f64) -> f64 {
    let d = (to - from).rem_euclid(360.0);
    if d > 180.0 { d - 360.0 } else { d }
}

// ------------------------------------------------------------------------------------------------ the rules

/// A rule constant that varies with the tone: its value on the darkest tone and on the lightest, blended by `t`.
#[derive(Copy, Clone)]
struct Span {
    deep: f64,
    light: f64,
}

impl Span {
    const fn new(deep: f64, light: f64) -> Self {
        Self { deep, light }
    }

    fn at(self, t: f64) -> f64 {
        self.deep + (self.light - self.deep) * t
    }
}

/// Base lightness of the lightest and the darkest tone: the ends of the range `t` is measured between.
const LIGHTEST_L: f64 = 0.895;
const DARKEST_L: f64 = 0.392;
/// Below this base lightness blush and lips go lighter than the skin (a red-brown); above it, darker (a pink).
const DEEP_BELOW_L: f64 = 0.5;
/// The hue of the default warm key light (OKLCH degrees); the rim colour leans this way.
const KEY_HUE: f64 = 68.0;
const RIM_KEY_PULL: f64 = 0.30;

const SHADE_HUE_TURN: Span = Span::new(6.0, 13.0);
const SHADE_LIGHTNESS: Span = Span::new(0.80, 0.915);
const SHADE_CHROMA_BOOST: Span = Span::new(0.012, 0.028);
const LINE_DARKER: Span = Span::new(0.09, 0.16);
const LINE_CHROMA: Span = Span::new(0.90, 1.0);
const LINE_HUE_OFFSET: f64 = -3.0;
const RIM_LIFT: Span = Span::new(0.26, 0.045);
const BLUSH_STEP: f64 = 0.12;
const BLUSH_CHROMA: Span = Span::new(0.105, 0.105);
const BLUSH_HUE: Span = Span::new(36.0, 24.0);
const LIP_STEP: f64 = 0.15;
const LIP_CHROMA: Span = Span::new(0.10, 0.115);
const LIP_HUE: Span = Span::new(30.0, 16.0);
const EAR_DARKER: Span = Span::new(0.02, 0.035);
const EAR_CHROMA: Span = Span::new(1.4, 1.6);
const EAR_HUE_TURN: Span = Span::new(-6.0, -10.0);

/// How warm, neutral or cool the skin leans at the same lightness.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Undertone {
    /// Golden: hue turned towards yellow.
    Warm,
    /// The tone file's base unchanged.
    #[default]
    Neutral,
    /// Rosy: hue turned towards pink, a little less chroma.
    Cool,
}

impl Undertone {
    /// The three undertones.
    pub const ALL: [Self; 3] = [Self::Warm, Self::Neutral, Self::Cool];

    /// Hue turn in degrees and chroma factor applied to the base.
    fn shift(self) -> (f64, f64) {
        match self {
            Self::Warm => (8.0, 1.10),
            Self::Neutral => (0.0, 1.0),
            Self::Cool => (-11.0, 0.92),
        }
    }

    /// The lower-case name, as written in ids and op arguments.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Neutral => "neutral",
            Self::Cool => "cool",
        }
    }
}

/// The renderer's skin treatment for a tone: what goes into `Shading::Toon.skin`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkinToneParams {
    /// Minimum shadow brightness as a fraction of the albedo, `0..=1`.
    pub floor: f32,
    /// How strongly coloured light is pulled back to the skin hue, `0..=1`.
    pub hue_keep: f32,
}

/// The treatment used only if the embedded library cannot be read, which the unit tests rule out: the middle of the
/// tuned range, so a skin still renders.
const UNTUNED: SkinToneParams = SkinToneParams { floor: 0.47, hue_keep: 0.65 };

impl SkinToneParams {
    /// The skin treatment for a skin colour, interpolated from the twelve tuned tones.
    ///
    /// The tones are placed by the OKLCH lightness of their lit `skin` colour. A colour between two of them gets the
    /// two tones' values, interpolated linearly by lightness; a colour lighter than the lightest tone or darker than
    /// the darkest gets that tone's values. At a tone's own colour the result is exactly its tuned values. Only
    /// lightness matters, so a custom or non-human skin colour is treated by how light it is. The table is built once
    /// from the embedded library, so a call allocates nothing.
    pub fn for_colour(skin: Colour) -> Self {
        let l = Oklch::from_colour(skin).l;
        // `above` is the number of keys at or below `l`, so the key before it is the closest one below and the key at
        // it the closest one above. Past either end only one neighbour exists and the treatment clamps to it.
        let keys = tone_keys();
        let above = keys.partition_point(|k| k.lightness <= l);
        match (above.checked_sub(1).and_then(|i| keys.get(i)), keys.get(above)) {
            (Some(below), Some(upper)) => {
                let t = (l - below.lightness) / (upper.lightness - below.lightness);
                below.params.lerp(upper.params, t)
            }
            (Some(only), None) | (None, Some(only)) => only.params,
            (None, None) => UNTUNED,
        }
    }

    /// The point `t` of the way from `self` (`t = 0`) to `to` (`t = 1`). At `t = 0` it is exactly `self`.
    fn lerp(self, to: Self, t: f64) -> Self {
        // `t` lies in `0..1` here, so narrowing to `f32` only rounds it.
        #[allow(clippy::cast_possible_truncation)]
        let t = t as f32;
        Self {
            floor: self.floor + (to.floor - self.floor) * t,
            hue_keep: self.hue_keep + (to.hue_keep - self.hue_keep) * t,
        }
    }
}

/// One library tone as [`SkinToneParams::for_colour`] sees it.
#[derive(Copy, Clone)]
struct ToneKey {
    /// OKLCH lightness of the tone's neutral-undertone `skin` colour, the colour the tone is defined by.
    lightness: f64,
    /// The tone's tuned treatment.
    params: SkinToneParams,
}

/// The lookup table for [`SkinToneParams::for_colour`]: the twelve tones by ascending lightness, built once.
///
/// Empty only if the embedded library fails to parse; the library unit test rules that out, and `for_colour` then
/// falls back to [`UNTUNED`].
fn tone_keys() -> &'static [ToneKey] {
    static KEYS: OnceLock<Vec<ToneKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys: Vec<ToneKey> = skin_tones()
            .unwrap_or(&[])
            .iter()
            .map(|tone| ToneKey {
                lightness: Oklch::from_colour(skin_ramp(tone, Undertone::Neutral).skin).l,
                params: tone.params,
            })
            .collect();
        keys.sort_by(|a, b| a.lightness.total_cmp(&b.lightness));
        keys
    })
}

impl From<SkinToneParams> for Skin {
    fn from(p: SkinToneParams) -> Self {
        Self { floor: p.floor, hue_keep: p.hue_keep }
    }
}

/// The palette slots of one skin: everything a toon-lit head, body and face read from the palette.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkinSlots {
    /// `skin`: the lit colour.
    pub skin: Colour,
    /// `skin_shade`: the colour the shadow side shows (see [`toon_shade`](Self::toon_shade) for the material).
    pub skin_shade: Colour,
    /// `skin_line`: the outline colour on skin.
    pub skin_line: Colour,
    /// `skin_rim`: the rim-light colour.
    pub skin_rim: Colour,
    /// `blush`: the cheek colour.
    pub blush: Colour,
    /// `lip`: the lip tint.
    pub lip: Colour,
    /// `ear_inner`: the inside of the ear.
    pub ear_inner: Colour,
}

impl SkinSlots {
    /// The slot names, in the order of [`entries`](Self::entries).
    pub const NAMES: [&'static str; 7] = ["skin", "skin_shade", "skin_line", "skin_rim", "blush", "lip", "ear_inner"];

    /// The `Toon::shade` multiplier that makes the shadow side show [`skin_shade`](Self::skin_shade): the ratio of the
    /// shade to the skin per channel in linear light, at most 1. The renderer multiplies the skin by `shade`, so using
    /// the slot colour itself would square it: harmless on light skin, black on deep skin.
    pub fn toon_shade(&self) -> Colour {
        let skin = self.skin.to_linear().max(Vec3::splat(1e-4));
        Colour::from_linear((self.skin_shade.to_linear() / skin).min(Vec3::ONE), 1.0)
    }

    /// Slot name and colour pairs.
    pub fn entries(&self) -> [(&'static str, Colour); 7] {
        let colours = [self.skin, self.skin_shade, self.skin_line, self.skin_rim, self.blush, self.lip, self.ear_inner];
        std::array::from_fn(|i| (Self::NAMES[i], colours[i]))
    }
}

/// One tone of the library.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinTone {
    /// `tone-01` (lightest) to `tone-12` (deepest).
    pub id: SkinToneId,
    /// A plain description: "Light medium", "Deep".
    pub name: String,
    /// The neutral-undertone base colour.
    pub base: Oklch,
    /// The renderer's skin treatment for this tone.
    pub params: SkinToneParams,
}

impl SkinTone {
    /// The id recorded in `CharacterDef.skin_tone`: the tone and its undertone, `tone-07-warm`.
    ///
    /// # Errors
    /// Only if the combined name is not a valid id (a tone id longer than 56 characters).
    pub fn record_id(&self, undertone: Undertone) -> Result<SkinToneId, omuzaki_core::id::SlugError> {
        SkinToneId::new(format!("{}-{}", self.id, undertone.as_str()))
    }
}

/// The palette slots for `tone` in `undertone`. The rules are in the module docs; they are pure and deterministic.
pub fn skin_ramp(tone: &SkinTone, undertone: Undertone) -> SkinSlots {
    let (turn, chroma) = undertone.shift();
    let skin = Oklch { l: tone.base.l, c: tone.base.c * chroma, h: tone.base.h + turn }.to_colour();
    // Work from what the 8-bit colour really is, so the relationships hold after rounding.
    let b = Oklch::from_colour(skin);
    let t = ((tone.base.l - DARKEST_L) / (LIGHTEST_L - DARKEST_L)).clamp(0.0, 1.0);

    let skin_shade =
        Oklch { l: b.l * SHADE_LIGHTNESS.at(t), c: b.c + SHADE_CHROMA_BOOST.at(t), h: b.h - SHADE_HUE_TURN.at(t) }
            .to_colour();
    let s = Oklch::from_colour(skin_shade);
    let skin_line =
        Oklch { l: s.l - LINE_DARKER.at(t), c: s.c * LINE_CHROMA.at(t), h: s.h + LINE_HUE_OFFSET }.to_colour();
    let skin_rim =
        Oklch { l: b.l + RIM_LIFT.at(t), c: b.c, h: b.h + RIM_KEY_PULL * hue_delta(b.h, KEY_HUE) }.to_colour();

    // Blush and lips must read against the skin on every tone: darker than light skin, lighter than deep skin.
    let direction = if b.l >= DEEP_BELOW_L { -1.0 } else { 1.0 };
    let (blush_l, lip_l) = (b.l + direction * BLUSH_STEP, b.l + direction * LIP_STEP);
    let blush = Oklch { l: blush_l, c: BLUSH_CHROMA.at(t), h: BLUSH_HUE.at(t) + 0.5 * turn }.to_colour();
    let lip = Oklch { l: lip_l, c: LIP_CHROMA.at(t), h: LIP_HUE.at(t) + 0.5 * turn }.to_colour();
    let ear_inner =
        Oklch { l: b.l - EAR_DARKER.at(t), c: b.c * EAR_CHROMA.at(t), h: b.h + EAR_HUE_TURN.at(t) }.to_colour();

    SkinSlots { skin, skin_shade, skin_line, skin_rim, blush, lip, ear_inner }
}

// ------------------------------------------------------------------------------------------------ the library

/// A tone file that does not parse or breaks a rule. The files are embedded, so this is a bug caught by the tests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("skin tone file {file}: {reason}")]
pub struct LibraryError {
    /// The file, relative to `library/skin-tones/`.
    pub file: &'static str,
    /// What is wrong with it.
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToneFile {
    id: SkinToneId,
    name: String,
    base: Oklch,
    skin: SkinToneParams,
}

macro_rules! tone_files {
    ($($n:literal),+ $(,)?) => {
        [$(($n, include_str!(concat!("../../library/skin-tones/", $n)))),+]
    };
}

/// The embedded tone files, lightest first.
const FILES: [(&str, &str); 12] = tone_files![
    "tone-01.toml",
    "tone-02.toml",
    "tone-03.toml",
    "tone-04.toml",
    "tone-05.toml",
    "tone-06.toml",
    "tone-07.toml",
    "tone-08.toml",
    "tone-09.toml",
    "tone-10.toml",
    "tone-11.toml",
    "tone-12.toml",
];

fn parse_tone(file: &'static str, source: &str) -> Result<SkinTone, LibraryError> {
    let bad = |reason: String| LibraryError { file, reason };
    let t: ToneFile = toml::from_str(source).map_err(|e| bad(e.to_string()))?;
    let unit = |v: f32| (0.0..=1.0).contains(&v);
    if !(0.0..=1.0).contains(&t.base.l) || !(0.0..=0.4).contains(&t.base.c) || !(0.0..360.0).contains(&t.base.h) {
        return Err(bad("base needs l in 0..=1, c in 0..=0.4 and h in 0..360".to_owned()));
    }
    if !unit(t.skin.floor) || !unit(t.skin.hue_keep) {
        return Err(bad("skin floor and hue_keep must be in 0..=1".to_owned()));
    }
    Ok(SkinTone { id: t.id, name: t.name, base: t.base, params: t.skin })
}

/// The twelve tones, lightest first.
///
/// # Errors
/// A [`LibraryError`] if an embedded tone file is malformed; the unit tests make sure none is.
pub fn skin_tones() -> Result<&'static [SkinTone], LibraryError> {
    static TONES: OnceLock<Result<Vec<SkinTone>, LibraryError>> = OnceLock::new();
    TONES
        .get_or_init(|| FILES.iter().map(|(file, source)| parse_tone(file, source)).collect())
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The tone called `id`.
///
/// # Errors
/// `Invalid` naming the field `tone` and listing the valid ids.
fn find_tone(id: &SkinToneId) -> Result<&'static SkinTone, OpError> {
    let tones = skin_tones().map_err(|e| OpError::internal(e.to_string()))?;
    tones.iter().find(|t| &t.id == id).ok_or_else(|| {
        let known = tones.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ");
        OpError::invalid("tone", format!("there is no skin tone '{id}'"))
            .hint(format!("the tones are {known}; ask `palette.skin_tones` for what each looks like"))
    })
}

// ------------------------------------------------------------------------------------------------ the ops

/// The part kinds whose `skin` colours follow the tone.
const SKIN_PART_KINDS: [&str; 2] = ["head", "body"];

/// Changes made against a working copy of one character, so a missing parent is created as one whole object in the
/// change that needs it (the patch rule that keeps undo exact) and a later change sees what an earlier one made.
struct CharacterEdit {
    root: DocPath,
    working: Value,
    patch: Patch,
}

impl CharacterEdit {
    fn new(root: DocPath, current: &Value) -> Self {
        Self { root, working: current.clone(), patch: Patch::new() }
    }

    /// Sets the value at `keys` below the character, creating any missing objects on the way.
    fn set(&mut self, keys: &[&str], value: Value) -> Result<(), OpError> {
        let Some((last, parents)) = keys.split_last() else {
            return Err(OpError::internal("an edit needs a path"));
        };
        let not_object = |at: &str| {
            OpError::conflict(format!("{}/{at} is not an object, so it can't hold a colour", self.root))
                .hint("fix the character file, or recreate the part")
        };
        // The deepest object that exists, and how many of the keys lead to it.
        let mut cur = &self.working;
        let mut depth = 0;
        for key in keys {
            match cur.get(*key) {
                Some(next) => {
                    cur = next;
                    depth += 1;
                }
                None => break,
            }
        }
        let (path_keys, before, after) = if depth == keys.len() {
            (keys, Some(cur.clone()), value.clone())
        } else {
            if !cur.is_object() {
                return Err(not_object(&keys[..depth].join("/")));
            }
            // Create the first missing object whole, with the rest of the path nested inside it.
            let nested = keys[depth + 1..].iter().rev().fold(value.clone(), |inner, key| {
                let mut map = Map::new();
                map.insert((*key).to_owned(), inner);
                Value::Object(map)
            });
            (&keys[..=depth], None, nested)
        };
        let path = path_keys.iter().fold(self.root.clone(), |p, k| p.join(*k));
        self.patch = std::mem::take(&mut self.patch).with(path, before, Some(after));

        let mut node = &mut self.working;
        for key in parents {
            let map = node.as_object_mut().ok_or_else(|| not_object(key))?;
            node = map.entry((*key).to_owned()).or_insert_with(|| Value::Object(Map::new()));
        }
        node.as_object_mut().ok_or_else(|| not_object(last))?.insert((*last).to_owned(), value);
        Ok(())
    }
}

fn slot_ref(name: &str) -> Result<ColourRef, OpError> {
    PaletteSlot::new(name).map(ColourRef::Slot).map_err(|e| OpError::internal(format!("slot '{name}': {e}")))
}

/// The head and body `skin` colours, every one pointing at a palette slot.
fn skin_colours() -> Result<Value, OpError> {
    let colours = SkinColours {
        base: slot_ref("skin")?,
        shade: slot_ref("skin_shade")?,
        line: slot_ref("skin_line")?,
        rim: slot_ref("skin_rim")?,
    };
    serde_json::to_value(colours).map_err(|e| OpError::internal(e.to_string()))
}

/// Sets a character's skin tone: the palette slots, the recorded tone, and every colour that should follow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSkinTone {
    /// The character.
    pub character: CharId,
    /// The tone, `tone-01` (very light) to `tone-12` (very deep).
    pub tone: SkinToneId,
    /// How warm or cool the skin leans; neutral when omitted.
    #[serde(default)]
    pub undertone: Undertone,
}

impl Op for SetSkinTone {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.set_skin_tone",
            summary: "Set a character's skin tone: the skin, shade, line, rim, blush, lip and ear palette slots, and \
                      the head, body and face colours that use them",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let tone = find_tone(&self.tone)?;
        let slots = skin_ramp(tone, self.undertone);
        let root = paths::character(&self.character);
        let current = ctx.doc.get(&root).ok_or_else(|| {
            OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("`character.create` makes one")
        })?;
        let def = CharacterDef::from_node(current).map_err(|e| {
            OpError::invalid("character", format!("character '{}' doesn't read as a character: {e}", self.character))
        })?;

        let mut edit = CharacterEdit::new(root, current);
        for (name, colour) in slots.entries() {
            edit.set(&["palette", name], json!(colour))?;
        }
        let record = tone.record_id(self.undertone).map_err(|e| OpError::internal(e.to_string()))?;
        edit.set(&["skin_tone"], json!(record))?;
        let skin = skin_colours()?;
        let skin_parts: Vec<&str> = def
            .parts
            .iter()
            .filter(|(_, part)| SKIN_PART_KINDS.contains(&part.kind.as_str()))
            .map(|(id, _)| id.as_str())
            .collect();
        for part in skin_parts.iter().copied() {
            edit.set(&["parts", part, "params", "skin"], skin.clone())?;
        }
        edit.set(&["face", "cheeks", "colour"], json!("@blush"))?;
        edit.set(&["face", "mouth", "lips", "colour"], json!("@lip"))?;

        let mut plan = Plan::edit(edit.patch);
        plan.output = Some(json!({
            "skin_tone": record,
            "slots": slots,
            "skin_params": tone.params,
            "parts": skin_parts,
        }));
        Ok(plan)
    }
}

register_op!(SetSkinTone);

/// Lists the skin-tone range with every slot colour, so a picker (or an agent) can choose by eye.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSkinTones {
    /// Only this undertone; all three when omitted.
    #[serde(default)]
    pub undertone: Option<Undertone>,
}

impl Op for ListSkinTones {
    fn meta() -> OpMeta {
        OpMeta {
            name: "palette.skin_tones",
            summary: "List the curated skin tones (very light to very deep) with the palette colours each gives",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let tones = skin_tones().map_err(|e| OpError::internal(e.to_string()))?;
        let mut listed = Vec::new();
        for tone in tones {
            for undertone in Undertone::ALL.into_iter().filter(|u| self.undertone.is_none_or(|only| only == *u)) {
                let record = tone.record_id(undertone).map_err(|e| OpError::internal(e.to_string()))?;
                listed.push(json!({
                    "tone": tone.id,
                    "name": tone.name,
                    "undertone": undertone,
                    "skin_tone": record,
                    "slots": skin_ramp(tone, undertone),
                    "skin_params": tone.params,
                }));
            }
        }
        Ok(Plan::answer(json!({ "tones": listed })))
    }
}

register_op!(ListSkinTones);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_library_is_twelve_valid_tones_from_light_to_deep() {
        let tones = skin_tones().unwrap();
        assert_eq!(tones.len(), 12);
        for (i, tone) in tones.iter().enumerate() {
            assert_eq!(tone.id.as_str(), format!("tone-{:02}", i + 1));
        }
        assert!(tones.windows(2).all(|w| w[0].base.l > w[1].base.l), "lightness falls tone by tone");
    }

    #[test]
    fn a_broken_tone_file_names_itself() {
        let e = parse_tone(
            "tone-99.toml",
            "id = \"tone-99\"\nname = \"x\"\n[base]\nl = 2.0\nc = 0.1\nh = 10.0\n[skin]\nfloor = 0.4\nhue_keep = 0.5\n",
        )
        .unwrap_err();
        assert_eq!(e.file, "tone-99.toml");
        assert!(e.reason.contains("base needs"), "{e}");
        assert!(parse_tone("tone-98.toml", "id = 3").is_err());
    }

    #[test]
    fn oklch_round_trips_an_srgb_colour() {
        for c in [Colour::rgb(249, 211, 197), Colour::rgb(72, 42, 27), Colour::rgb(255, 255, 255)] {
            assert_eq!(Oklch::from_colour(c).to_colour(), c);
        }
    }

    #[test]
    fn an_out_of_gamut_colour_keeps_its_hue_and_lightness() {
        let wild = Oklch { l: 0.5, c: 0.35, h: 30.0 };
        let fitted = Oklch::from_colour(wild.to_colour());
        assert!(fitted.c < wild.c);
        assert!((fitted.l - 0.5).abs() < 0.01 && hue_delta(fitted.h, 30.0).abs() < 2.0, "{fitted:?}");
    }

    #[test]
    fn hue_deltas_take_the_short_way_round() {
        assert!((hue_delta(350.0, 10.0) - 20.0).abs() < 1e-9);
        assert!((hue_delta(10.0, 350.0) + 20.0).abs() < 1e-9);
        assert!(hue_delta(40.0, 40.0).abs() < 1e-9);
    }

    #[test]
    fn the_tone_id_recorded_on_a_character_carries_the_undertone() {
        let tone = &skin_tones().unwrap()[6];
        assert_eq!(tone.record_id(Undertone::Warm).unwrap().as_str(), "tone-07-warm");
    }
}
