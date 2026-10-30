//! The character's look (spec v3 §21, LK-06): a preset plus the person's overrides, and the transform that draws the
//! parts' materials in it.
//!
//! # The look node
//!
//! `characters/<id>/look` (`look.toml`) is a [`LookDef`]: a preset id ([`DEFAULT_PRESET`] when absent) and
//! [`LookParams`] overrides on top of it. [`LookDef::resolve`] gives the look to draw: the preset's parameters with the
//! overrides laid over them, the person's settings always beating the preset's ([`LookParams::layered_over`]).
//!
//! Presets are data ([`LookPreset`]). A crate ships them as files and registers them once, with
//! [`register_look_presets!`](crate::register_look_presets!) (no central list: adding a preset is adding a file). [`look_presets`] lists every
//! registered preset and [`find_look_preset`] finds one, naming the nearest id when one is misspelt.
//!
//! # What a look changes, and what it never does
//!
//! A look is about *how* surfaces are drawn, never *what colour they are*:
//! - per material class ([`MaterialClass`]: skin, hair, cloth, metal, eyes, plus `all` for every class, see
//!   [`ClassLook`]) it scales the toon terminator's softness and the rim, swaps in a shading program or tunes the
//!   parameters of one, sets the outline width (scaled, or one uniform weight) and colour (the material's own line, or
//!   one flat colour), and turns shadow reception on or off;
//! - for the frame ([`SceneLook`]) it turns bloom and depth of field on or off and sets grain and a colour grade. Light
//!   rigs stay separate: the bloom's numbers are the rig's, and the stage reads this part when it stages a shot.
//!
//! It never touches a material's base colour, base texture, shade colour, emissive, overlays, alpha, sidedness or
//! vertex colours, so the palette's colours reach the screen as the parts made them under every look. **The painted
//! face opts out by construction:** it is an overlay of the head material, so it keeps its own colours, and only the
//! lighting of the skin it sits on follows the look.
//!
//! # Why the transform runs at build time
//!
//! [`BuiltCharacter::apply_look`](crate::BuiltCharacter::apply_look) runs on the built character, not inside the
//! renderer, because **exports must carry the look**: a VRM's `MToon` fields are gathered from the built materials, so
//! a character exported in cel-flat looks cel-flat in any `MToon` renderer, and the renderer and the exports can never
//! disagree. It runs on materials only and always starts from what the parts built
//! ([`BuiltCharacter::part_materials`](crate::BuiltCharacter::part_materials)), so a look switch never rebuilds geometry
//! (the stage updates the materials that changed) and never compounds: cel-flat and then soft-sd gives back the
//! soft-sd bytes. It is a pure function of the parts' materials, their classes and the look, so it is deterministic.

use std::collections::BTreeMap;

use omuzaki_core::colour::Colour;
use omuzaki_core::id::Slug;
use omuzaki_doc_api::Value;
use omuzaki_render_api::{ColourGrade, Outline, ParamValue, ProgramParams, Shading};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::character::nearest_name;
use crate::geometry::MaterialSpec;
use crate::hair::HAIR_PROGRAM;
use crate::ids::LookPresetId;
use crate::kind::PartCategory;

/// The preset a character without a look node is drawn in: the parts' own materials, the product's default look
/// (ADR 0001). Its file is `soft-sd.toml` in `omuzaki-char`'s look library.
pub const DEFAULT_PRESET: &str = "soft-sd";

/// How much darker than its surface a line the look *adds* is, as a factor on linear light (LK-02: a darker shade of
/// the material, never black). Measured on the prototype's soft-SD materials (`js/character/mira/parts/*.js`): the
/// line's luminance over the base's has a median of 0.27 and a mean of 0.28 across ten materials (skin, hair, cloth,
/// leather, gold). Lines a part drew keep the part's colour.
pub const DERIVED_LINE: f32 = 0.28;

// ------------------------------------------------------------------------------------------------ classes

/// The kinds of surface a look addresses separately (LK-06).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaterialClass {
    /// Faces, bodies, ears, hands: the materials with the skin treatment.
    Skin,
    /// Hair, drawn with the hair program or toon.
    Hair,
    /// Garments, footwear and most accessories: the default for anything that isn't skin or hair.
    Cloth,
    /// Buttons, buckles, jewellery. Only a part's declaration makes a material metal.
    Metal,
    /// Modelled eyes (an imported model's, a creature's). Painted eyes are part of the face overlay and keep their own
    /// colours. Only a part's declaration makes a material an eye.
    Eyes,
}

impl MaterialClass {
    /// Every class, in order.
    pub const ALL: [Self; 5] = [Self::Skin, Self::Hair, Self::Cloth, Self::Metal, Self::Eyes];

    /// The class's name, as files and parameter paths spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skin => "skin",
            Self::Hair => "hair",
            Self::Cloth => "cloth",
            Self::Metal => "metal",
            Self::Eyes => "eyes",
        }
    }

    /// The class of a material its part did not declare one for
    /// ([`PartOutput::material_classes`](crate::PartOutput::material_classes)): skin if it has the skin treatment,
    /// hair if it is drawn with the hair program; otherwise by the part's category (body and head parts are skin, hair
    /// parts hair, everything else cloth). Metal and eyes are never inferred: nothing in a material says so reliably.
    pub fn infer(category: PartCategory, spec: &MaterialSpec) -> Self {
        match &spec.shading {
            Shading::Toon(toon) if toon.skin.is_some() => return Self::Skin,
            Shading::Program { name, .. } if name.as_str() == HAIR_PROGRAM => return Self::Hair,
            _ => {}
        }
        match category {
            PartCategory::Body | PartCategory::Head => Self::Skin,
            PartCategory::Hair => Self::Hair,
            PartCategory::Garment | PartCategory::Footwear | PartCategory::Accessory => Self::Cloth,
        }
    }
}

// ------------------------------------------------------------------------------------------------ parameters

/// How a look colours outlines.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutlineColour {
    /// The material's own line (a darker shade of its surface, LK-02). Where the part drew no line and the look adds
    /// one, the surface colour darkened by [`DERIVED_LINE`].
    Material,
    /// One colour for every line of the class.
    Flat(Colour),
}

/// A shading program to draw a class with instead of what its parts chose, as a look layer holds it: both parts are
/// optional, so a layer can tune one parameter of the program a lower layer chose. After resolution a program with
/// parameters always has a name ([`LookDef::resolve`] refuses one without).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProgramLook {
    /// The program's registered name (`gloss`, `painterly`). A renderer that lacks it draws the default toon look, as
    /// the render contract says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<Slug>,
    /// Its parameters; a missing one takes the program's default. The renderer checks names and ranges when it
    /// creates the material.
    #[serde(skip_serializing_if = "ProgramParams::is_empty")]
    pub params: ProgramParams,
}

impl ProgramLook {
    /// The field names, as parameter paths spell them.
    pub const FIELDS: [&'static str; 2] = ["name", "params"];

    /// True when it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These settings over `base`'s, leaf by leaf: the name is this layer's, else the base's, and the parameters merge
    /// per parameter, these winning. A layer that names a *different* program than the base's drops the base's
    /// parameters, which belong to the program they were written for.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        let same_program = self.name.is_none() || self.name == base.name || base.name.is_none();
        let mut params = if same_program { base.params.clone() } else { ProgramParams::new() };
        params.extend(self.params.iter().map(|(k, v)| (k.clone(), *v)));
        Self { name: first(self.name.as_ref(), base.name.as_ref()), params }
    }
}

/// What a look sets for one class of material. Every field is optional: unset keeps what the part built, so the empty
/// value changes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ClassLook {
    /// Multiplies the toon terminator's softness (`Toon::softness`), the result kept in `0..=1`: 0 gives a hard
    /// two-tone edge, 1 the part's own. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub softness_scale: Option<f32>,
    /// Multiplies the toon rim light's strength: 0 turns rims off. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rim_scale: Option<f32>,
    /// Draws the class with this shading program instead of its own shading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramLook>,
    /// Parameters for materials drawn with a given program, by program name, laid over the material's own: cel-flat
    /// turns the hair program's highlights off this way. A material drawn with another program is left alone, so a
    /// parameter never reaches a program that doesn't declare it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub program_params: BTreeMap<Slug, ProgramParams>,
    /// One uniform line width, metres at `Outline::REFERENCE_DISTANCE`, replacing each part's width. It also adds a
    /// line where the part drew none; 0 removes the lines. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_width: Option<f32>,
    /// Multiplies the line width (after `outline_width`); 0 removes the lines. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_width_scale: Option<f32>,
    /// How the lines are coloured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline_colour: Option<OutlineColour>,
    /// Whether shadow maps darken the class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receives_shadows: Option<bool>,
    /// Where the class sits on the style axis (#2607), `0..=1`: `1` the cel look the parts built (exactly), `0` physical
    /// (subsurface skin, fibre-lit hair, a conductor for metal, a physically based surface with cloth sheen for the
    /// rest), and every value between a continuous blend. Lines thin with it and are gone at `0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<f32>,
}

/// What a look sets for the whole frame (LK-05). Every field is optional: unset keeps what the shot's recipe chose.
/// Light rigs stay separate: when the look turns bloom on, its strength, radius and threshold are the rig's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SceneLook {
    /// Bloom on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bloom: Option<bool>,
    /// Depth of field on or off; the stage focuses on what the camera looks at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<bool>,
    /// Film grain strength, `0..=1`; 0 turns it off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grain: Option<f32>,
    /// A colour grade, leaf by leaf: a leaf left unset takes the preset's, then the neutral value.
    #[serde(skip_serializing_if = "GradeOverride::is_empty")]
    pub grade: GradeOverride,
}

/// A colour grade as a look layer holds it: every leaf optional, so a layer can set `saturation` alone and leave
/// `contrast` to the layer below. A full [`ColourGrade`] exists only after resolution ([`GradeOverride::to_grade`]).
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GradeOverride {
    /// 1 leaves colour alone, 0 is greyscale, above 1 is more vivid. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
    /// 1 leaves contrast alone; above 1 steepens the curve around mid grey. `>= 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
}

impl GradeOverride {
    /// The field names, as parameter paths spell them.
    pub const FIELDS: [&'static str; 2] = ["saturation", "contrast"];

    /// True when it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These leaves over `base`'s: a leaf set here wins.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        Self {
            saturation: first(self.saturation.as_ref(), base.saturation.as_ref()),
            contrast: first(self.contrast.as_ref(), base.contrast.as_ref()),
        }
    }

    /// The grade to apply: `None` when no leaf is set (the shot's recipe keeps its own), otherwise the set leaves with
    /// the neutral value (1, which leaves colour alone) for the rest.
    pub fn to_grade(&self) -> Option<ColourGrade> {
        (!self.is_empty())
            .then(|| ColourGrade { saturation: self.saturation.unwrap_or(1.0), contrast: self.contrast.unwrap_or(1.0) })
    }
}

/// A look's parameters: per material class, for every class (`all`), and for the frame. Presets hold one and the look
/// node holds one as overrides; every field is optional, so the empty value is "the parts' own materials".
///
/// For a material of class `c` the settings are `c`'s over `all`'s, field by field ([`LookParams::class`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LookParams {
    /// Settings for every class; a class's own settings win over these.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub all: ClassLook,
    /// Skin.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub skin: ClassLook,
    /// Hair.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub hair: ClassLook,
    /// Cloth.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub cloth: ClassLook,
    /// Metal.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub metal: ClassLook,
    /// Eyes.
    #[serde(skip_serializing_if = "ClassLook::is_empty")]
    pub eyes: ClassLook,
    /// The frame.
    #[serde(skip_serializing_if = "SceneLook::is_empty")]
    pub scene: SceneLook,
}

/// `characters/<id>/look` (`look.toml`): the preset a character is drawn in and the person's overrides on top of it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookDef {
    /// The preset; [`DEFAULT_PRESET`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<LookPresetId>,
    /// The person's settings, laid over the preset's.
    #[serde(default, skip_serializing_if = "LookParams::is_empty")]
    pub overrides: LookParams,
}

/// A named look, shipped as a data file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookPreset {
    /// Its id, which is also its file name (`cel-flat` in `cel-flat.toml`).
    pub id: LookPresetId,
    /// The name people see (`Cel flat`).
    pub name: String,
    /// One sentence on what it looks like.
    pub summary: String,
    /// Where its numbers come from: a design target, a measurement, a generator's defaults. Never a reference
    /// character's name.
    pub origin: String,
    /// What it sets.
    #[serde(default)]
    pub params: LookParams,
}

/// Why a look could not be read, found or used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookError {
    /// No registered preset has this id.
    #[error("there is no look preset `{id}`{}; the presets are: {}", nearest_hint(.nearest.as_deref()), .known.join(", "))]
    UnknownPreset {
        /// The id asked for.
        id: String,
        /// The registered id closest to it, when one is close enough to be a typo.
        nearest: Option<String>,
        /// Every registered id, sorted.
        known: Vec<String>,
    },
    /// A crate's preset files don't load (a test of the crate catches this before it ships).
    #[error("a look preset file doesn't load: {0}")]
    Source(String),
    /// Two presets share an id.
    #[error("two look presets are called `{0}`: preset ids must be unique across crates")]
    DuplicatePreset(String),
    /// A parameter is unknown, malformed or out of range. `path` is its dotted path (`cloth.outline_width_scale`).
    #[error("look parameter `{path}`: {reason}")]
    Invalid {
        /// The parameter at fault.
        path: String,
        /// What is wrong, and the nearest valid name for an unknown one.
        reason: String,
    },
}

fn nearest_hint(nearest: Option<&str>) -> String {
    nearest.map(|n| format!(" (did you mean `{n}`?)")).unwrap_or_default()
}

fn invalid(path: impl Into<String>, reason: impl Into<String>) -> LookError {
    LookError::Invalid { path: path.into(), reason: reason.into() }
}

/// A scale or width: finite and `>= 0`.
fn check_non_negative(path: &str, value: Option<f32>) -> Result<(), LookError> {
    match value {
        Some(v) if !(v.is_finite() && v >= 0.0) => Err(invalid(path, format!("{v} must be a finite number >= 0"))),
        _ => Ok(()),
    }
}

fn check_params(path: &str, params: &ProgramParams) -> Result<(), LookError> {
    for (name, value) in params {
        let finite = match value {
            ParamValue::Float(v) => v.is_finite(),
            ParamValue::Vec2(v) => v.is_finite(),
            ParamValue::Vec3(v) => v.is_finite(),
            ParamValue::Colour(_) | ParamValue::Bool(_) => true,
        };
        if !finite {
            return Err(invalid(format!("{path}.{name}"), "program parameters must be finite"));
        }
    }
    Ok(())
}

/// The first `Some` of `over` and `base`.
fn first<T: Clone>(over: Option<&T>, base: Option<&T>) -> Option<T> {
    over.or(base).cloned()
}

impl ClassLook {
    /// The field names, as parameter paths spell them.
    pub const FIELDS: [&'static str; 9] = [
        "softness_scale",
        "rim_scale",
        "program",
        "program_params",
        "outline_width",
        "outline_width_scale",
        "outline_colour",
        "receives_shadows",
        "style",
    ];

    /// True when it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These settings over `base`, field by field: a field set here wins; `program_params` merge per program and per
    /// parameter, these winning.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        let mut program_params = base.program_params.clone();
        for (program, params) in &self.program_params {
            program_params.entry(program.clone()).or_default().extend(params.iter().map(|(k, v)| (k.clone(), *v)));
        }
        Self {
            softness_scale: first(self.softness_scale.as_ref(), base.softness_scale.as_ref()),
            rim_scale: first(self.rim_scale.as_ref(), base.rim_scale.as_ref()),
            program: match (&self.program, &base.program) {
                (Some(mine), Some(theirs)) => Some(mine.over(theirs)),
                (mine, theirs) => mine.clone().or_else(|| theirs.clone()),
            },
            program_params,
            outline_width: first(self.outline_width.as_ref(), base.outline_width.as_ref()),
            outline_width_scale: first(self.outline_width_scale.as_ref(), base.outline_width_scale.as_ref()),
            outline_colour: first(self.outline_colour.as_ref(), base.outline_colour.as_ref()),
            receives_shadows: first(self.receives_shadows.as_ref(), base.receives_shadows.as_ref()),
            style: first(self.style.as_ref(), base.style.as_ref()),
        }
    }

    /// Checks every number; `section` names the class in the error (`cloth`).
    ///
    /// # Errors
    /// [`LookError::Invalid`] naming the first bad field.
    pub fn validate(&self, section: &str) -> Result<(), LookError> {
        check_non_negative(&format!("{section}.softness_scale"), self.softness_scale)?;
        check_non_negative(&format!("{section}.rim_scale"), self.rim_scale)?;
        check_non_negative(&format!("{section}.outline_width"), self.outline_width)?;
        check_non_negative(&format!("{section}.outline_width_scale"), self.outline_width_scale)?;
        if let Some(style) = self.style
            && !(0.0..=1.0).contains(&style)
        {
            return Err(invalid(format!("{section}.style"), format!("{style} is outside 0 (physical) to 1 (cel)")));
        }
        if let Some(program) = &self.program {
            check_params(&format!("{section}.program.params"), &program.params)?;
        }
        for (program, params) in &self.program_params {
            check_params(&format!("{section}.program_params.{program}"), params)?;
        }
        Ok(())
    }

    /// `spec` drawn in this look. Only the shading's softness, rim and program, the outline and shadow reception can
    /// change; every colour the part chose stays (see the [module docs](self)). With nothing set, the result equals
    /// `spec` exactly.
    pub fn apply(&self, spec: &MaterialSpec) -> MaterialSpec {
        let mut out = spec.clone();
        if let Some(program) = &self.program
            && let Some(name) = &program.name
        {
            // Retaining the program keeps its parameters (a hair program's palette-resolved root, mid and tip colours),
            // with the look's own parameters over them; only a different program starts from the look's parameters.
            match &mut out.shading {
                Shading::Program { name: current, params } if current == name => {
                    params.extend(program.params.iter().map(|(k, v)| (k.clone(), *v)));
                }
                shading => *shading = Shading::Program { name: name.clone(), params: program.params.clone() },
            }
        }
        match &mut out.shading {
            Shading::Toon(toon) => {
                if let Some(scale) = self.softness_scale {
                    toon.softness = (toon.softness * scale).clamp(0.0, 1.0);
                }
                if let Some(scale) = self.rim_scale {
                    toon.rim.strength *= scale;
                }
            }
            Shading::Program { name, params } => {
                if let Some(extra) = self.program_params.get(name) {
                    params.extend(extra.iter().map(|(k, v)| (k.clone(), *v)));
                }
            }
            Shading::Unlit => {}
        }
        out.outline = self.outline(spec);
        if let Some(receives) = self.receives_shadows {
            out.receives_shadows = receives;
        }
        if let Some(style) = self.style
            && style < 1.0
        {
            styled(&mut out, style);
        }
        out
    }

    /// [`apply`](Self::apply), then the renderer's own material check on the result, so a look that is valid on its
    /// own but overflows on this material (a width of 1e30 times a scale of 1e30) is refused here instead of stored.
    /// A material the look leaves as it was is not checked: it is the part's business.
    ///
    /// # Errors
    /// [`LookError::Invalid`] at `path` with the renderer's reason.
    pub fn try_apply(&self, spec: &MaterialSpec, path: &str) -> Result<MaterialSpec, LookError> {
        let drawn = self.apply(spec);
        if drawn != *spec
            && let Err(e) = drawn.validate()
        {
            return Err(invalid(path, format!("the look makes a material the renderer rejects: {e}")));
        }
        Ok(drawn)
    }

    /// The line `spec` gets. Untouched when no outline field is set, so the empty look keeps the part's bytes; a width
    /// the look sets to 0 removes the line, while a part's own zero-width line is the part's business.
    fn outline(&self, spec: &MaterialSpec) -> Option<Outline> {
        if self.outline_width.is_none() && self.outline_width_scale.is_none() && self.outline_colour.is_none() {
            return spec.outline;
        }
        // Only a zero the look itself sets removes a line. A part's own zero-width line stays one, so a scale of 1 (Soft
        // SD) is the identity on every line.
        // Both are validated `>= 0`, so `<= 0` is exactly zero.
        if self.outline_width.is_some_and(|w| w <= 0.0) || self.outline_width_scale.is_some_and(|s| s <= 0.0) {
            return None;
        }
        let width = self.outline_width.or(spec.outline.map(|o| o.width))? * self.outline_width_scale.unwrap_or(1.0);
        let colour = match self.outline_colour {
            Some(OutlineColour::Flat(colour)) => colour,
            Some(OutlineColour::Material) | None => spec.outline.map_or_else(|| derived_line(spec.base), |o| o.colour),
        };
        Some(Outline { colour, width })
    }
}

/// The skin program (`omuzaki-render`'s `skin`): subsurface-scattered skin at the physical end of the style axis.
pub const SKIN_PROGRAM: &str = "skin";
/// The physical program (`omuzaki-render`'s `physical`): every other surface at the physical end of the style axis.
pub const PHYSICAL_PROGRAM: &str = "physical";
/// The metal program, which has a physical end of its own.
const METAL_PROGRAM: &str = "metal";

/// Moves `spec` along the style axis to `style` (`< 1`): a toon material becomes the skin program (if the toon has the
/// skin treatment) or the physical one, carrying its toon as the cel end (`cel-*`), so the blend meets the toon exactly
/// at `1`; the programs with a physical end (hair, metal, skin, physical) get the style; lines thin with it.
fn styled(spec: &mut MaterialSpec, style: f32) {
    let put = |params: &mut ProgramParams, name: &str, value: ParamValue| {
        if let Ok(key) = Slug::new(name) {
            params.insert(key, value);
        }
    };
    match &mut spec.shading {
        Shading::Toon(toon) => {
            let program = if toon.skin.is_some() { SKIN_PROGRAM } else { PHYSICAL_PROGRAM };
            let mut params = ProgramParams::new();
            put(&mut params, "cel-shade", ParamValue::Colour(toon.shade));
            put(&mut params, "cel-shift", ParamValue::Float(toon.shift));
            put(&mut params, "cel-softness", ParamValue::Float(toon.softness));
            put(&mut params, "cel-rim", ParamValue::Colour(toon.rim.colour));
            put(&mut params, "cel-rim-power", ParamValue::Float(toon.rim.power));
            put(&mut params, "cel-rim-strength", ParamValue::Float(toon.rim.strength));
            put(&mut params, "cel-emissive", ParamValue::Colour(toon.emissive));
            put(&mut params, "cel-skin", ParamValue::Float(if toon.skin.is_some() { 1.0 } else { 0.0 }));
            if let Some(skin) = toon.skin {
                put(&mut params, "cel-skin-floor", ParamValue::Float(skin.floor));
                put(&mut params, "cel-skin-hue", ParamValue::Float(skin.hue_keep));
            } else {
                // Cloth for the most part: a matte, softly sheened weave.
                put(&mut params, "roughness", ParamValue::Float(0.75));
                put(&mut params, "sheen", ParamValue::Float(0.35));
            }
            put(&mut params, "style", ParamValue::Float(style));
            if let Ok(name) = Slug::new(program) {
                spec.shading = Shading::Program { name, params };
            }
        }
        Shading::Program { name, params }
            if [HAIR_PROGRAM, METAL_PROGRAM, SKIN_PROGRAM, PHYSICAL_PROGRAM].contains(&name.as_str()) =>
        {
            put(params, "style", ParamValue::Float(style));
        }
        Shading::Program { .. } | Shading::Unlit => {}
    }
    spec.outline = spec.outline.filter(|_| style > 0.0).map(|o| Outline { width: o.width * style, ..o });
}

/// A darker shade of `base` for a line the look adds: [`DERIVED_LINE`] of its linear light, opaque.
fn derived_line(base: Colour) -> Colour {
    Colour::from_linear(base.to_linear() * DERIVED_LINE, 1.0)
}

impl SceneLook {
    /// The field names, as parameter paths spell them.
    pub const FIELDS: [&'static str; 4] = ["bloom", "depth_of_field", "grain", "grade"];

    /// True when it sets nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// These settings over `base`, field by field.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        Self {
            bloom: first(self.bloom.as_ref(), base.bloom.as_ref()),
            depth_of_field: first(self.depth_of_field.as_ref(), base.depth_of_field.as_ref()),
            grain: first(self.grain.as_ref(), base.grain.as_ref()),
            grade: self.grade.over(&base.grade),
        }
    }

    /// Checks every number.
    ///
    /// # Errors
    /// [`LookError::Invalid`] naming the first bad field.
    pub fn validate(&self) -> Result<(), LookError> {
        if let Some(grain) = self.grain
            && !(0.0..=1.0).contains(&grain)
        {
            return Err(invalid("scene.grain", format!("{grain} must be in 0..=1")));
        }
        check_non_negative("scene.grade.saturation", self.grade.saturation)?;
        check_non_negative("scene.grade.contrast", self.grade.contrast)?;
        Ok(())
    }
}

impl LookParams {
    /// The section names: `all`, the classes, `scene`.
    pub const SECTIONS: [&'static str; 7] = ["all", "skin", "hair", "cloth", "metal", "eyes", "scene"];

    /// True when it sets nothing: the parts' own materials and the recipe's own frame.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn section(&self, class: MaterialClass) -> &ClassLook {
        match class {
            MaterialClass::Skin => &self.skin,
            MaterialClass::Hair => &self.hair,
            MaterialClass::Cloth => &self.cloth,
            MaterialClass::Metal => &self.metal,
            MaterialClass::Eyes => &self.eyes,
        }
    }

    fn section_mut(&mut self, class: MaterialClass) -> &mut ClassLook {
        match class {
            MaterialClass::Skin => &mut self.skin,
            MaterialClass::Hair => &mut self.hair,
            MaterialClass::Cloth => &mut self.cloth,
            MaterialClass::Metal => &mut self.metal,
            MaterialClass::Eyes => &mut self.eyes,
        }
    }

    /// The settings for materials of `class`: its own over `all`.
    pub fn class(&self, class: MaterialClass) -> ClassLook {
        self.section(class).over(&self.all)
    }

    /// The look of the person's settings (`self`) over a preset's (`base`): **anything set here beats anything set
    /// there**, so a person's `all` beats the preset's `hair`. For each class the result holds, in order of priority,
    /// this class, this `all`, the base's class, the base's `all`; its `all` is this `all` over the base's.
    #[must_use]
    pub fn layered_over(&self, base: &Self) -> Self {
        let mut out = Self { all: self.all.over(&base.all), scene: self.scene.over(&base.scene), ..Self::default() };
        for class in MaterialClass::ALL {
            let mine = self.section(class).over(&self.all);
            *out.section_mut(class) = mine.over(&base.class(class));
        }
        out
    }

    /// Checks every number of every section.
    ///
    /// # Errors
    /// [`LookError::Invalid`] naming the first bad parameter.
    pub fn validate(&self) -> Result<(), LookError> {
        self.all.validate("all")?;
        for class in MaterialClass::ALL {
            self.section(class).validate(class.as_str())?;
        }
        self.scene.validate()
    }

    /// The person's settings over a preset's, validated and ready to draw.
    fn resolved_over(&self, preset: &Self) -> Result<Self, LookError> {
        let look = self.layered_over(preset);
        look.validate()?;
        // A parameter override with no program to tune (the preset has none and the person named none) can't be drawn.
        for class in MaterialClass::ALL {
            if let Some(program) = &look.section(class).program
                && program.name.is_none()
            {
                let section = class.as_str();
                return Err(invalid(
                    format!("{section}.program.name"),
                    format!("`{section}.program` has parameters but no program: set `{section}.program.name`"),
                ));
            }
        }
        Ok(look)
    }

    /// Sets the parameter at the dotted `path` to `value`, or removes it when `value` is `None` (so the preset's value
    /// shows through again). `value` is what the field holds in a look file, as JSON: `0.5`, `true`, `"material"`,
    /// `{"flat": "#2a1f1a"}`, `{"float": 0.0}` for a program parameter.
    ///
    /// Paths are `<section>.<field>` and deeper for structured fields: `cloth.outline_width_scale`, `scene.bloom`,
    /// `scene.grade.saturation`, a program swap as `hair.program` (`{"name": "gloss"}`) and one of its parameters as
    /// `hair.program.params.highlight-size`, a parameter of the hair's own program as
    /// `hair.program_params.hair.ring-strength`. An unknown section or field is refused with the nearest valid name.
    ///
    /// # Errors
    /// [`LookError::Invalid`] for an unknown or malformed path, a value of the wrong shape, or a value out of range.
    pub fn set_param(&mut self, path: &str, value: Option<Value>) -> Result<(), LookError> {
        let segments = Self::check_path(path)?;
        let mut tree = serde_json::to_value(&*self).map_err(|e| invalid(path, e.to_string()))?;
        if let Some(value) = value {
            if value.is_null() {
                return Err(invalid(path, "a look file can't store null: omit the value to remove the parameter"));
            }
            insert(&mut tree, &segments, value);
        } else {
            remove(&mut tree, &segments);
        }
        let edited: Self = serde_json::from_value(tree).map_err(|e| invalid(path, e.to_string()))?;
        edited.validate()?;
        *self = edited;
        Ok(())
    }
}

impl LookParams {
    /// Splits a parameter path ([`set_param`](Self::set_param)) and checks all of it, so a caller can tell a wrong
    /// name from a wrong value. Every fixed segment is checked against its type's field list; the free-form keys (a
    /// program's name and its parameters' names) only for slug syntax.
    ///
    /// # Errors
    /// [`LookError::Invalid`] for a path without a field, an unknown section or field at any depth (naming the nearest
    /// valid one), a segment under a field that has none, or a malformed free-form key.
    pub fn check_path(path: &str) -> Result<Vec<&str>, LookError> {
        parse_path(path)
    }
}

/// Splits a parameter path and checks every segment against the look's shape, naming the nearest valid name.
fn parse_path(path: &str) -> Result<Vec<&str>, LookError> {
    let segments: Vec<&str> = path.split('.').collect();
    if segments.len() < 2 || segments.iter().any(|s| s.is_empty()) {
        return Err(invalid(
            path,
            "write it as <section>.<field>, for example cloth.outline_width_scale or scene.bloom",
        ));
    }
    let known = |name: &str, names: &[&'static str], what: &str| -> Result<(), LookError> {
        if names.contains(&name) {
            return Ok(());
        }
        let hint = nearest_name(name, names.iter().copied())
            .map_or_else(|| format!("expected one of {}", names.join(", ")), |n| format!("did you mean `{n}`?"));
        Err(invalid(path, format!("there is no {what} `{name}`: {hint}")))
    };
    // A program's or a parameter's name: any slug, since the registry of programs lives in the renderer.
    let key = |name: &str, what: &str| -> Result<(), LookError> {
        Slug::new(name).map(drop).map_err(|e| invalid(path, format!("`{name}` isn't a valid {what} name: {e}")))
    };
    let leaf = |at: usize, field: &str| -> Result<(), LookError> {
        match segments.get(at) {
            None => Ok(()),
            Some(extra) => Err(invalid(path, format!("`{field}` has no sub-field `{extra}`"))),
        }
    };
    let (section, field) = (segments[0], segments[1]);
    known(section, &LookParams::SECTIONS, "section")?;
    if section == "scene" {
        known(field, &SceneLook::FIELDS, "field")?;
        if field == "grade" {
            if let Some(leaf_name) = segments.get(2) {
                known(leaf_name, &GradeOverride::FIELDS, "field")?;
                leaf(3, leaf_name)?;
            }
        } else {
            leaf(2, field)?;
        }
        return Ok(segments);
    }
    known(field, &ClassLook::FIELDS, "field")?;
    match field {
        "program" => {
            if let Some(sub) = segments.get(2) {
                known(sub, &ProgramLook::FIELDS, "field")?;
                if *sub == "params" {
                    if let Some(param) = segments.get(3) {
                        key(param, "parameter")?;
                    }
                    leaf(4, "params")?;
                } else {
                    leaf(3, sub)?;
                }
            }
        }
        "program_params" => {
            if let Some(program) = segments.get(2) {
                key(program, "program")?;
                if let Some(param) = segments.get(3) {
                    key(param, "parameter")?;
                }
                leaf(4, "program_params")?;
            }
        }
        _ => leaf(2, field)?,
    }
    Ok(segments)
}

/// Sets `value` at `path` in a JSON tree of tables, creating the tables on the way.
fn insert(tree: &mut Value, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut node = tree;
    for segment in parents {
        if !node.is_object() {
            *node = Value::Object(serde_json::Map::new());
        }
        node = match node {
            Value::Object(map) => {
                map.entry((*segment).to_owned()).or_insert_with(|| Value::Object(serde_json::Map::new()))
            }
            _ => return,
        };
    }
    if !node.is_object() {
        *node = Value::Object(serde_json::Map::new());
    }
    if let Value::Object(map) = node {
        map.insert((*last).to_owned(), value);
    }
}

/// Removes the value at `path` from a JSON tree, and every table the removal leaves empty.
fn remove(tree: &mut Value, path: &[&str]) {
    let Some((first, rest)) = path.split_first() else { return };
    let Value::Object(map) = tree else { return };
    if rest.is_empty() {
        map.remove(*first);
        return;
    }
    if let Some(child) = map.get_mut(*first) {
        remove(child, rest);
        if child.as_object().is_some_and(serde_json::Map::is_empty) {
            map.remove(*first);
        }
    }
}

impl LookDef {
    /// The preset's id: the stored one, or [`DEFAULT_PRESET`].
    pub fn preset_id(&self) -> &str {
        self.preset.as_ref().map_or(DEFAULT_PRESET, LookPresetId::as_str)
    }

    /// The look node of a character, from the value at `characters/<id>/look` (`None` when the character has none,
    /// which is the default look).
    ///
    /// # Errors
    /// [`LookError::Invalid`] (path `look`) when the node doesn't match the look format, naming the field.
    pub fn from_node(node: Option<&Value>) -> Result<Self, LookError> {
        node.map_or_else(
            || Ok(Self::default()),
            |value| serde_json::from_value(value.clone()).map_err(|e| invalid("look", e.to_string())),
        )
    }

    /// The look to draw: the preset's parameters with the overrides over them ([`LookParams::layered_over`]),
    /// validated.
    ///
    /// # Errors
    /// [`LookError::UnknownPreset`] with the nearest id, a preset source error, or [`LookError::Invalid`] for an
    /// override out of range.
    pub fn resolve(&self) -> Result<LookParams, LookError> {
        let preset = find_look_preset(self.preset_id())?;
        self.overrides.resolved_over(&preset.params)
    }
}

// ------------------------------------------------------------------------------------------------ the registry

/// A crate's look presets: one registration per crate ([`register_look_presets!`](crate::register_look_presets!)), whatever the number of files.
pub struct LookPresetSource {
    /// Every preset the crate ships, parsed. An error names the file that doesn't load.
    pub load: fn() -> Result<Vec<LookPreset>, String>,
}

inventory::collect!(LookPresetSource);

/// Registers a crate's look presets: `$load` is a `fn() -> Result<Vec<LookPreset>, String>` that parses the crate's
/// preset files. Call it once per crate, next to the files' embedding.
///
/// ```ignore
/// omuzaki_char_api::register_look_presets!(load_presets);
/// ```
#[macro_export]
macro_rules! register_look_presets {
    ($load:path) => {
        $crate::__inventory::submit! { $crate::LookPresetSource { load: $load } }
        $crate::__record_registration!("look_presets", $load);
    };
}

/// Every registered preset, sorted by id.
///
/// # Errors
/// [`LookError::Source`] if a crate's files don't load, [`LookError::DuplicatePreset`] if two share an id.
pub fn look_presets() -> Result<Vec<LookPreset>, LookError> {
    let mut all = Vec::new();
    for source in inventory::iter::<LookPresetSource> {
        all.extend((source.load)().map_err(LookError::Source)?);
    }
    all.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = all.windows(2).find(|pair| pair[0].id == pair[1].id) {
        return Err(LookError::DuplicatePreset(pair[0].id.to_string()));
    }
    Ok(all)
}

/// The registered preset `id`.
///
/// # Errors
/// [`LookError::UnknownPreset`] naming the nearest id and every id, or the errors of [`look_presets`].
pub fn find_look_preset(id: &str) -> Result<LookPreset, LookError> {
    let all = look_presets()?;
    if let Some(found) = all.iter().find(|p| p.id.as_str() == id) {
        return Ok(found.clone());
    }
    let known: Vec<String> = all.iter().map(|p| p.id.to_string()).collect();
    let nearest = nearest_name(id, known.iter().map(String::as_str)).map(str::to_owned);
    Err(LookError::UnknownPreset { id: id.to_owned(), nearest, known })
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec2;
    use omuzaki_face_api::{FaceLayerId, PlanarWindow};
    use omuzaki_render_api::{AlphaMode, Rim, Skin, Toon};
    use serde_json::json;

    use super::*;
    use crate::geometry::FaceOverlay;
    use crate::ids::TextureName;

    fn slug(s: &str) -> Slug {
        Slug::new(s).unwrap()
    }

    fn toon(softness: f32, rim: f32) -> MaterialSpec {
        let mut spec = MaterialSpec::toon(Colour::hex(0x00f9_d3c5));
        spec.shading = Shading::Toon(Toon {
            shade: Colour::hex(0x00f2_b4ab),
            shift: -0.28,
            softness,
            rim: Rim { colour: Colour::hex(0x00ff_d9cf), power: 3.0, strength: rim },
            emissive: Colour::hex(0x0001_0203),
            skin: Some(Skin::default()),
        });
        spec.outline = Some(Outline { colour: Colour::hex(0x00c9_8a80), width: 0.0014 });
        spec
    }

    fn hair() -> MaterialSpec {
        let mut spec = MaterialSpec::toon(Colour::WHITE);
        let params =
            [("ring-strength", ParamValue::Float(0.6)), ("root", ParamValue::Colour(Colour::hex(0x0011_2233)))];
        spec.shading = Shading::Program {
            name: slug(HAIR_PROGRAM),
            params: params.into_iter().map(|(k, v)| (slug(k), v)).collect(),
        };
        spec.vertex_colours = true;
        spec.double_sided = true;
        spec
    }

    /// A material with every field a look must never touch set to something other than its default.
    fn decorated() -> MaterialSpec {
        let mut spec = toon(0.28, 0.1);
        spec.base_texture = Some(TextureName::new("plaid").unwrap());
        spec.overlays = vec![FaceOverlay {
            layer: FaceLayerId::Base,
            window: PlanarWindow { min: Vec2::new(-0.1, 0.0), max: Vec2::new(0.1, 0.2), fade_from: 0.0, fade_to: 0.05 },
            strength: 1.0,
        }];
        spec.alpha = AlphaMode::Mask { cutoff: 0.4 };
        spec.vertex_colours = true;
        spec
    }

    #[test]
    fn the_empty_look_changes_nothing() {
        let unlit = MaterialSpec { shading: Shading::Unlit, ..MaterialSpec::toon(Colour::BLACK) };
        for spec in [toon(0.28, 0.1), hair(), decorated(), unlit] {
            assert_eq!(ClassLook::default().apply(&spec), spec);
            assert_eq!(LookParams::default().class(MaterialClass::Cloth).apply(&spec), spec);
        }
    }

    #[test]
    fn scales_change_softness_and_rim_and_keep_softness_in_range() {
        let look = ClassLook { softness_scale: Some(0.0), rim_scale: Some(0.0), ..ClassLook::default() };
        let Shading::Toon(t) = look.apply(&toon(0.28, 0.1)).shading else { panic!("still toon") };
        assert_eq!((t.softness, t.rim.strength), (0.0, 0.0));
        let look = ClassLook { softness_scale: Some(10.0), rim_scale: Some(2.0), ..ClassLook::default() };
        let Shading::Toon(t) = look.apply(&toon(0.28, 0.1)).shading else { panic!("still toon") };
        assert_eq!((t.softness, t.rim.strength), (1.0, 0.2));
    }

    #[test]
    fn outlines_scale_take_one_width_add_lines_and_go_away_at_zero() {
        let line = |look: ClassLook, spec: &MaterialSpec| look.apply(spec).outline;
        let skin = toon(0.28, 0.1);
        let own = skin.outline.unwrap();
        let scaled = line(ClassLook { outline_width_scale: Some(2.0), ..ClassLook::default() }, &skin).unwrap();
        assert_eq!((scaled.colour, scaled.width), (own.colour, own.width * 2.0));
        let uniform = ClassLook { outline_width: Some(0.002), ..ClassLook::default() };
        assert_eq!(line(uniform.clone(), &skin).unwrap().width, 0.002);
        // A material without a line gets one in a darker shade of its surface.
        let bare = MaterialSpec::toon(Colour::rgb(200, 100, 50));
        let added = line(uniform, &bare).unwrap();
        assert_eq!(added.width, 0.002);
        assert_eq!(added.colour, derived_line(bare.base));
        assert!(added.colour.r < 200 && added.colour.g < 100 && added.colour.b < 50, "darker: {:?}", added.colour);
        // A part's own zero-width line stays a zero-width line: Soft SD's scale of 1 (with its colour mode) is the
        // identity, and no scale turns it into "no line" (the review case).
        let mut hairline = toon(0.28, 0.1);
        hairline.outline = Some(Outline { colour: Colour::BLACK, width: 0.0 });
        let soft_sd = ClassLook {
            softness_scale: Some(1.0),
            rim_scale: Some(1.0),
            outline_width_scale: Some(1.0),
            outline_colour: Some(OutlineColour::Material),
            ..ClassLook::default()
        };
        assert_eq!(soft_sd.apply(&hairline), hairline);
        let doubled = ClassLook { outline_width_scale: Some(2.0), ..ClassLook::default() };
        assert_eq!(doubled.apply(&hairline).outline, Some(Outline { colour: Colour::BLACK, width: 0.0 }));
        let material = ClassLook { outline_colour: Some(OutlineColour::Material), ..ClassLook::default() };
        assert_eq!(material.apply(&hairline), hairline);
        // A zero the look sets does remove the line, through either field.
        assert_eq!(line(ClassLook { outline_width: Some(0.0), ..ClassLook::default() }, &skin), None);
        // Scaling alone never adds a line, and zero removes one.
        assert_eq!(line(ClassLook { outline_width_scale: Some(3.0), ..ClassLook::default() }, &bare), None);
        assert_eq!(line(ClassLook { outline_width_scale: Some(0.0), ..ClassLook::default() }, &skin), None);
        let flat = OutlineColour::Flat(Colour::hex(0x002a_1f1a));
        let coloured = line(ClassLook { outline_colour: Some(flat), ..ClassLook::default() }, &skin).unwrap();
        assert_eq!((coloured.colour, coloured.width), (Colour::hex(0x002a_1f1a), own.width));
    }

    #[test]
    fn programs_swap_in_and_parameters_reach_only_their_own_program() {
        let gloss = ClassLook {
            program: Some(ProgramLook {
                name: Some(slug("gloss")),
                params: [(slug("highlight-size"), ParamValue::Float(0.2))].into(),
            }),
            ..ClassLook::default()
        };
        let swapped = gloss.apply(&toon(0.28, 0.1));
        assert_eq!(
            swapped.shading,
            Shading::Program { name: slug("gloss"), params: [(slug("highlight-size"), ParamValue::Float(0.2))].into() }
        );

        let flat_hair = ClassLook {
            program_params: [(slug(HAIR_PROGRAM), [(slug("ring-strength"), ParamValue::Float(0.0))].into())].into(),
            ..ClassLook::default()
        };
        let Shading::Program { params, .. } = flat_hair.apply(&hair()).shading else { panic!("still a program") };
        assert_eq!(params[&slug("ring-strength")], ParamValue::Float(0.0));
        assert_eq!(params[&slug("root")], ParamValue::Colour(Colour::hex(0x0011_2233)), "other parameters stay");
        assert_eq!(flat_hair.apply(&toon(0.28, 0.1)), toon(0.28, 0.1), "a toon material has no hair program");
    }

    #[test]
    fn retaining_the_program_name_keeps_its_parameters() {
        let named = |params: ProgramParams| ClassLook {
            program: Some(ProgramLook { name: Some(slug(HAIR_PROGRAM)), params }),
            ..ClassLook::default()
        };
        let Shading::Program { params: own, .. } = hair().shading else { panic!("a program") };
        let Shading::Program { params, .. } = named(ProgramParams::default()).apply(&hair()).shading else {
            panic!("still a program")
        };
        assert_eq!(params, own);
        assert_eq!(params[&slug("root")], ParamValue::Colour(Colour::hex(0x0011_2233)));
        // The look's own parameters win over the retained ones; the rest stay.
        let look = named([(slug("ring-strength"), ParamValue::Float(0.0))].into());
        let Shading::Program { params, .. } = look.apply(&hair()).shading else { panic!("still a program") };
        assert_eq!(params[&slug("ring-strength")], ParamValue::Float(0.0));
        assert_eq!(params[&slug("root")], ParamValue::Colour(Colour::hex(0x0011_2233)));
    }

    #[test]
    fn a_look_that_overflows_a_material_is_refused() {
        let huge = ClassLook { outline_width: Some(1e30), outline_width_scale: Some(1e30), ..ClassLook::default() };
        assert!(huge.validate("cloth").is_ok(), "each number is finite on its own");
        let err = huge.try_apply(&toon(0.28, 0.1), "materials.skin").unwrap_err();
        assert!(matches!(&err, LookError::Invalid { path, .. } if path == "materials.skin"), "{err:?}");
        let rim = ClassLook { rim_scale: Some(f32::MAX), ..ClassLook::default() };
        assert!(rim.try_apply(&toon(0.28, f32::MAX), "m").is_err(), "rim strength overflows");
        // The boundary: the largest finite width is accepted, one past it is not.
        let edge = ClassLook { outline_width: Some(f32::MAX), outline_width_scale: Some(1.0), ..ClassLook::default() };
        assert_eq!(edge.try_apply(&toon(0.28, 0.1), "m").unwrap().outline.unwrap().width, f32::MAX);
        let past = ClassLook { outline_width_scale: Some(2.0), ..edge };
        assert!(past.try_apply(&toon(0.28, 0.1), "m").is_err());
        // A look that changes nothing never fails, even on a material the renderer would reject.
        let mut broken = toon(0.28, 0.1);
        broken.outline = Some(Outline { colour: Colour::BLACK, width: f32::INFINITY });
        assert_eq!(ClassLook::default().try_apply(&broken, "m").unwrap(), broken);
    }

    #[test]
    fn no_look_touches_a_colour_the_part_chose() {
        let mut rng = omuzaki_core::rng::Rng::new(1006);
        let flat = Colour::hex(0x0012_3456);
        for _ in 0..200 {
            let mut pick = |p: f32| rng.range_f32(0.0, 1.0) < p;
            let look = ClassLook {
                softness_scale: pick(0.5).then_some(0.0),
                rim_scale: pick(0.5).then_some(3.0),
                program: pick(0.3).then(|| ProgramLook { name: Some(slug("gloss")), params: ProgramParams::new() }),
                outline_width: pick(0.5).then_some(0.004),
                outline_width_scale: pick(0.5).then_some(0.5),
                outline_colour: pick(0.5).then_some(OutlineColour::Flat(flat)),
                receives_shadows: pick(0.5).then_some(false),
                ..ClassLook::default()
            };
            for spec in [decorated(), hair()] {
                let drawn = look.apply(&spec);
                let (base, texture, overlays) = (drawn.base, drawn.base_texture.clone(), drawn.overlays.clone());
                assert_eq!((base, texture, overlays), (spec.base, spec.base_texture.clone(), spec.overlays.clone()));
                assert_eq!(
                    (drawn.alpha, drawn.double_sided, drawn.vertex_colours),
                    (spec.alpha, spec.double_sided, spec.vertex_colours)
                );
                if let (Shading::Toon(a), Shading::Toon(b)) = (&drawn.shading, &spec.shading) {
                    assert_eq!(
                        (a.shade, a.emissive, a.skin, a.rim.colour),
                        (b.shade, b.emissive, b.skin, b.rim.colour)
                    );
                }
            }
        }
    }

    #[test]
    fn a_class_takes_its_own_settings_over_all() {
        let look = LookParams {
            all: ClassLook { softness_scale: Some(0.0), rim_scale: Some(0.0), ..ClassLook::default() },
            hair: ClassLook { softness_scale: Some(0.5), ..ClassLook::default() },
            ..LookParams::default()
        };
        assert_eq!(look.class(MaterialClass::Hair).softness_scale, Some(0.5));
        assert_eq!(look.class(MaterialClass::Hair).rim_scale, Some(0.0));
        assert_eq!(look.class(MaterialClass::Cloth).softness_scale, Some(0.0));
    }

    #[test]
    fn the_persons_settings_beat_the_presets_even_across_sections() {
        let preset = LookParams {
            all: ClassLook { rim_scale: Some(0.5), ..ClassLook::default() },
            hair: ClassLook { outline_width_scale: Some(0.5), softness_scale: Some(0.2), ..ClassLook::default() },
            scene: SceneLook { bloom: Some(true), grain: Some(0.1), ..SceneLook::default() },
            ..LookParams::default()
        };
        let person = LookParams {
            all: ClassLook { outline_width_scale: Some(1.2), ..ClassLook::default() },
            hair: ClassLook { softness_scale: Some(0.7), ..ClassLook::default() },
            scene: SceneLook { bloom: Some(false), ..SceneLook::default() },
            ..LookParams::default()
        };
        let look = person.layered_over(&preset);
        let hair = look.class(MaterialClass::Hair);
        assert_eq!(hair.outline_width_scale, Some(1.2), "the person's `all` beats the preset's `hair`");
        assert_eq!(hair.softness_scale, Some(0.7));
        assert_eq!(hair.rim_scale, Some(0.5), "unset by the person: the preset's");
        assert_eq!(look.class(MaterialClass::Skin).outline_width_scale, Some(1.2));
        assert_eq!((look.scene.bloom, look.scene.grain), (Some(false), Some(0.1)));
        assert_eq!(
            LookParams::default().layered_over(&preset).class(MaterialClass::Hair),
            preset.class(MaterialClass::Hair)
        );
    }

    #[test]
    fn validation_names_the_parameter() {
        let cases: [(LookParams, &str); 5] = [
            (
                LookParams {
                    cloth: ClassLook { softness_scale: Some(-1.0), ..ClassLook::default() },
                    ..LookParams::default()
                },
                "cloth.softness_scale",
            ),
            (
                LookParams {
                    all: ClassLook { outline_width: Some(f32::NAN), ..ClassLook::default() },
                    ..LookParams::default()
                },
                "all.outline_width",
            ),
            (
                LookParams { scene: SceneLook { grain: Some(2.0), ..SceneLook::default() }, ..LookParams::default() },
                "scene.grain",
            ),
            (
                LookParams {
                    scene: SceneLook {
                        grade: GradeOverride { saturation: Some(-1.0), contrast: Some(1.0) },
                        ..SceneLook::default()
                    },
                    ..LookParams::default()
                },
                "scene.grade.saturation",
            ),
            (
                LookParams {
                    hair: ClassLook {
                        program_params: [(slug("hair"), [(slug("width"), ParamValue::Float(f32::INFINITY))].into())]
                            .into(),
                        ..ClassLook::default()
                    },
                    ..LookParams::default()
                },
                "hair.program_params.hair.width",
            ),
        ];
        for (look, path) in cases {
            match look.validate() {
                Err(LookError::Invalid { path: got, .. }) => assert_eq!(got, path),
                other => panic!("{path}: {other:?}"),
            }
        }
        assert_eq!(LookParams::default().validate(), Ok(()));
    }

    #[test]
    fn set_param_edits_one_field_and_refuses_typos_with_the_nearest_name() {
        let mut look = LookParams::default();
        look.set_param("cloth.outline_width_scale", Some(json!(0.5))).unwrap();
        look.set_param("scene.bloom", Some(json!(false))).unwrap();
        look.set_param("all.outline_colour", Some(json!({"flat": "#2a1f1a"}))).unwrap();
        look.set_param("hair.program_params.hair.ring-strength", Some(json!({"float": 0.0}))).unwrap();
        assert_eq!(look.cloth.outline_width_scale, Some(0.5));
        assert_eq!(look.scene.bloom, Some(false));
        assert_eq!(look.all.outline_colour, Some(OutlineColour::Flat(Colour::hex(0x002a_1f1a))));
        assert_eq!(look.hair.program_params[&slug("hair")][&slug("ring-strength")], ParamValue::Float(0.0));

        look.set_param("hair.program_params.hair.ring-strength", None).unwrap();
        assert!(look.hair.is_empty(), "removing the last parameter leaves no empty tables: {:?}", look.hair);
        look.set_param("cloth.outline_width_scale", None).unwrap();
        assert!(look.cloth.is_empty());

        let refusal =
            |path: &str, value: Value| LookParams::default().set_param(path, Some(value)).unwrap_err().to_string();
        assert!(refusal("cloth.outline_widht_scale", json!(1)).contains("did you mean `outline_width_scale`?"));
        assert!(refusal("clth.rim_scale", json!(1)).contains("did you mean `cloth`?"));
        assert!(refusal("scene.blom", json!(true)).contains("did you mean `bloom`?"));
        assert!(refusal("cloth", json!(1)).contains("<section>.<field>"));
        assert!(refusal("cloth.rim_scale", json!("lots")).contains("cloth.rim_scale"));
        assert!(refusal("cloth.rim_scale", json!(-2)).contains(">= 0"));
        assert!(refusal("scene.grade.vibrance", json!(1)).contains("vibrance"));
        assert!(refusal("cloth.rim_scale", Value::Null).contains("null"));
    }

    fn preset_with_contrast() -> LookParams {
        LookParams {
            scene: SceneLook {
                grade: GradeOverride { contrast: Some(1.4), ..GradeOverride::default() },
                ..SceneLook::default()
            },
            ..LookParams::default()
        }
    }

    #[test]
    fn a_grade_leaf_is_set_and_cleared_on_its_own() {
        // On a fresh look, `saturation` alone is a valid edit.
        let mut look = LookParams::default();
        look.set_param("scene.grade.saturation", Some(json!(0.5))).unwrap();
        assert_eq!(look.scene.grade, GradeOverride { saturation: Some(0.5), contrast: None });

        // Over a preset that sets `contrast`, the preset's contrast survives; clearing shows the preset's value again.
        let preset = preset_with_contrast();
        assert_eq!(
            look.scene.grade.over(&preset.scene.grade).to_grade(),
            Some(ColourGrade { saturation: 0.5, contrast: 1.4 })
        );
        look.set_param("scene.grade.saturation", None).unwrap();
        assert!(look.scene.is_empty(), "the empty grade table goes with its last leaf: {look:?}");
        assert_eq!(
            look.layered_over(&preset).scene.grade.to_grade(),
            Some(ColourGrade { saturation: 1.0, contrast: 1.4 })
        );

        // Both leaves set, one cleared: the other stays.
        look.set_param("scene.grade.saturation", Some(json!(0.5))).unwrap();
        look.set_param("scene.grade.contrast", Some(json!(2.0))).unwrap();
        look.set_param("scene.grade.saturation", None).unwrap();
        assert_eq!(look.scene.grade, GradeOverride { saturation: None, contrast: Some(2.0) });

        // Clearing a valid path that isn't set is a no-op; the whole grade can be set and cleared as a table too.
        look.set_param("scene.grade.saturation", None).unwrap();
        look.set_param("scene.grade", Some(json!({"saturation": 0.0}))).unwrap();
        assert_eq!(look.scene.grade, GradeOverride { saturation: Some(0.0), contrast: None });
        look.set_param("scene.grade", None).unwrap();
        assert!(look.scene.grade.is_empty());
        assert_eq!(GradeOverride::default().to_grade(), None, "no leaf set leaves the recipe's grade alone");
    }

    #[test]
    fn a_misspelt_path_is_refused_whatever_the_look_holds_and_whether_it_sets_or_clears() {
        let mut full = LookParams::default();
        full.set_param("scene.grade.saturation", Some(json!(0.5))).unwrap();
        full.set_param("scene.grade.contrast", Some(json!(1.5))).unwrap();
        full.set_param("hair.program", Some(json!({"name": "gloss", "params": {"width": {"float": 1.0}}}))).unwrap();
        full.set_param("hair.program_params.hair.ring-strength", Some(json!({"float": 0.0}))).unwrap();
        for look in [LookParams::default(), full] {
            for path in [
                "scene.grade.saturaton",
                "scene.bloom.strength",
                "scene.grade.saturation.x",
                "scene.grade.",
                "hair.program.nmae",
                "hair.program.name.x",
                "hair.program.params.Bad Key",
                "hair.program.params.width.x",
                "hair.program_params.Hair.x",
                "hair.program_params.hair.ring-strength.x",
                "hair.program_params.hair.UPPER",
                "cloth.rim_scale.x",
            ] {
                let mut copy = look.clone();
                for value in [Some(json!(1)), None] {
                    let action = if value.is_some() { "set" } else { "clear" };
                    let err = copy.set_param(path, value).expect_err(&format!("{action} {path}"));
                    assert!(err.to_string().contains(path), "{action} {path}: {err}");
                    assert_eq!(copy, look, "{action} {path} must not touch the look");
                }
            }
        }
        let hint = |path: &str| LookParams::default().set_param(path, None).unwrap_err().to_string();
        assert!(hint("scene.grade.saturaton").contains("did you mean `saturation`?"));
        assert!(hint("hair.program.nmae").contains("did you mean `name`?"));
    }

    fn registered_program_preset(name: Option<&str>, params: &[(&str, f32)]) -> LookParams {
        LookParams {
            hair: ClassLook {
                program: Some(ProgramLook {
                    name: name.map(slug),
                    params: params.iter().map(|(k, v)| (slug(k), ParamValue::Float(*v))).collect(),
                }),
                ..ClassLook::default()
            },
            ..LookParams::default()
        }
    }

    #[test]
    fn a_program_parameter_is_set_and_cleared_on_its_own_over_the_presets_program() {
        let preset = registered_program_preset(Some("gloss"), &[("highlight-size", 0.2), ("sheen", 0.5)]);
        let resolved = |look: &LookParams| look.layered_over(&preset).hair.program.unwrap();

        // Set one parameter on empty overrides: the name comes from the preset, the sibling survives.
        let mut look = LookParams::default();
        look.set_param("hair.program.params.highlight-size", Some(json!({"float": 0.9}))).unwrap();
        assert_eq!(look.hair.program.as_ref().unwrap().name, None);
        let program = resolved(&look);
        assert_eq!(program.name, Some(slug("gloss")));
        assert_eq!(program.params[&slug("highlight-size")], ParamValue::Float(0.9));
        assert_eq!(program.params[&slug("sheen")], ParamValue::Float(0.5));

        // Set a second, clear the first: the preset's value shows through again, the other override stays.
        look.set_param("hair.program.params.sheen", Some(json!({"float": 0.1}))).unwrap();
        look.set_param("hair.program.params.highlight-size", None).unwrap();
        let program = resolved(&look);
        assert_eq!(program.params[&slug("highlight-size")], ParamValue::Float(0.2));
        assert_eq!(program.params[&slug("sheen")], ParamValue::Float(0.1));

        // Clearing the last leaf removes the whole table.
        look.set_param("hair.program.params.sheen", None).unwrap();
        assert!(look.hair.is_empty(), "{look:?}");

        // Naming the same program keeps the preset's parameters; naming another drops them.
        look.set_param("hair.program.name", Some(json!("gloss"))).unwrap();
        assert_eq!(resolved(&look).params.len(), 2);
        look.set_param("hair.program.name", Some(json!("painterly"))).unwrap();
        let program = resolved(&look);
        assert_eq!((program.name, program.params.len()), (Some(slug("painterly")), 0));
    }

    #[test]
    fn program_params_set_and_clear_per_parameter_over_the_presets() {
        let mut preset = LookParams::default();
        preset.set_param("hair.program_params.hair.ring-strength", Some(json!({"float": 0.8}))).unwrap();
        preset.set_param("hair.program_params.hair.width", Some(json!({"float": 0.3}))).unwrap();
        let resolved = |look: &LookParams| look.layered_over(&preset).hair.program_params[&slug("hair")].clone();

        let mut look = LookParams::default();
        look.set_param("hair.program_params.hair.ring-strength", Some(json!({"float": 0.0}))).unwrap();
        let params = resolved(&look);
        assert_eq!(params[&slug("ring-strength")], ParamValue::Float(0.0));
        assert_eq!(params[&slug("width")], ParamValue::Float(0.3), "the sibling keeps the preset's value");

        look.set_param("hair.program_params.hair.ring-strength", None).unwrap();
        assert!(look.hair.is_empty(), "{look:?}");
        assert_eq!(resolved(&look)[&slug("ring-strength")], ParamValue::Float(0.8), "the preset shows through again");
    }

    #[test]
    fn a_program_parameter_with_no_program_to_tune_fails_at_resolution_naming_the_name() {
        let mut look = LookParams::default();
        look.set_param("hair.program.params.sheen", Some(json!({"float": 0.1}))).unwrap();
        // A preset with no hair program leaves the override nothing to tune.
        match look.resolved_over(&LookParams::default()) {
            Err(LookError::Invalid { path, .. }) => assert_eq!(path, "hair.program.name"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn valid_deep_paths_into_programs_are_accepted() {
        for path in [
            "hair.program",
            "hair.program.name",
            "hair.program.params",
            "hair.program.params.highlight-size",
            "hair.program_params",
            "hair.program_params.hair",
            "hair.program_params.hair.ring-strength",
            "scene.grade",
            "scene.grade.contrast",
        ] {
            assert!(LookParams::check_path(path).is_ok(), "{path}");
        }
    }

    #[test]
    fn an_override_with_an_unknown_grade_field_is_refused_on_read() {
        let err = serde_json::from_value::<LookParams>(json!({"scene": {"grade": {"saturaton": 1.0}}})).unwrap_err();
        assert!(err.to_string().contains("saturaton"), "{err}");
    }

    #[test]
    fn looks_round_trip_and_leave_unset_fields_out() {
        let look = LookDef {
            preset: Some(LookPresetId::new("cel-flat").unwrap()),
            overrides: LookParams {
                cloth: ClassLook {
                    outline_colour: Some(OutlineColour::Material),
                    program: Some(ProgramLook { name: Some(slug("gloss")), params: ProgramParams::new() }),
                    ..ClassLook::default()
                },
                scene: SceneLook {
                    grade: GradeOverride { saturation: Some(1.1), contrast: Some(1.0) },
                    ..SceneLook::default()
                },
                ..LookParams::default()
            },
        };
        let stored = serde_json::to_value(&look).unwrap();
        assert_eq!(
            stored,
            json!({"preset": "cel-flat", "overrides": {
                "cloth": {"program": {"name": "gloss"}, "outline_colour": "material"},
                "scene": {"grade": {"saturation": 1.1_f32, "contrast": 1.0}}}})
        );
        assert_eq!(LookDef::from_node(Some(&stored)).unwrap(), look);
        assert_eq!(serde_json::to_value(LookDef::default()).unwrap(), json!({}), "no nulls: a look file is TOML");
        assert_eq!(LookDef::from_node(None).unwrap(), LookDef::default());
        assert_eq!(LookDef::default().preset_id(), DEFAULT_PRESET);
        let err = LookDef::from_node(Some(&json!({"preset": "soft-sd", "overides": {}}))).unwrap_err();
        assert!(err.to_string().contains("overides"), "{err}");
    }

    #[test]
    fn classes_are_inferred_from_the_skin_treatment_the_hair_program_and_the_category() {
        let plain = MaterialSpec::toon(Colour::WHITE);
        assert_eq!(MaterialClass::infer(PartCategory::Garment, &toon(0.2, 0.1)), MaterialClass::Skin);
        assert_eq!(MaterialClass::infer(PartCategory::Accessory, &hair()), MaterialClass::Hair);
        assert_eq!(MaterialClass::infer(PartCategory::Head, &plain), MaterialClass::Skin);
        assert_eq!(MaterialClass::infer(PartCategory::Body, &plain), MaterialClass::Skin);
        assert_eq!(MaterialClass::infer(PartCategory::Hair, &plain), MaterialClass::Hair);
        for category in [PartCategory::Garment, PartCategory::Footwear, PartCategory::Accessory] {
            assert_eq!(MaterialClass::infer(category, &plain), MaterialClass::Cloth);
        }
        let names: Vec<_> = MaterialClass::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names, LookParams::SECTIONS[1..6]);
    }

    #[test]
    fn schemas_generate() {
        for schema in [schemars::schema_for!(LookDef), schemars::schema_for!(LookPreset)] {
            assert!(serde_json::to_value(schema).is_ok());
        }
    }
}
