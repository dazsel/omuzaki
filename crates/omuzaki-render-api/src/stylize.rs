//! The drawn look of the whole frame: the screen-space half of a stylised, animated-film pipeline.
//!
//! Stylised film pipelines (the comic look of halftone and ink, the cel look of posterised tones, the printed look of
//! paper and a lens) split the work between the surfaces, which choose how light falls on them (the `stylized` shading
//! program, toon), and a pass over the finished frame that draws *how it was printed or inked*. This is that pass. It
//! runs in display space after the tone curve, the colour grade and the backdrop, on straight colour, so it is drawn
//! the same over a gradient or a picture and never changes alpha: a transparent pixel stays transparent for OBS.
//!
//! Every size is a fraction of the output's **height**, so a look reads the same at 720p, 1080p and 4K, and every
//! pattern is a pure function of the pixel's position (no noise texture, no clock), so a frame re-renders to the same
//! bytes.
//!
//! In order, per pixel:
//! 1. **Lens fringe** (chromatic aberration): red is read nearer the corners and blue nearer the centre.
//! 2. **Posterize**: the display luma is cut into `levels` flat steps and the colour scaled to it, so hue survives (a
//!    per-channel posterize would shift hues at every step).
//! 3. **Split toning**: shadows pulled toward one tint and highlights toward another.
//! 4. **Halftone**: a printed dot screen, darker where the picture is darker, multiplied in as ink.
//! 5. **Hatching**: ink lines in the dark tones, a second crossing layer in the darkest.
//! 6. **Paper**: a soft fibre grain and a paper tint, multiplied over everything.
//! 7. **Vignette**: the corners fall off.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Colour, RenderError};

/// The most tone levels a posterize asks for: above it the steps are no longer seen in 8 bits.
pub const MAX_POSTERIZE: u32 = 64;

/// A printed dot screen (amplitude-modulated halftone): dots on a rotated grid, larger where the picture is darker.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Halftone {
    /// The distance between dot centres, as a fraction of the output height, `0.001..=0.1` (0.006 is a fine
    /// comic-book screen at 1080p: about 6.5 px).
    #[schemars(range(min = 0.001, max = 0.1))]
    pub cell: f32,
    /// The screen's angle, degrees (print uses 15, 45 or 75).
    pub angle_deg: f32,
    /// The ink, multiplied over the picture where a dot is (a darker shade of the picture keeps its hue; black inks).
    pub ink: Colour,
    /// How much of the ink is laid, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
    /// The display luma, `0..=1`, at and above which no dots are printed: 0.5 screens the shade side only, 1 the whole
    /// picture.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub threshold: f32,
}

impl Default for Halftone {
    fn default() -> Self {
        Self { cell: 0.006, angle_deg: 45.0, ink: Colour::hex(0x0026_1d33), strength: 0.6, threshold: 0.55 }
    }
}

/// Ink hatching in the dark tones: parallel lines, and a crossing layer where it is darker still.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Hatching {
    /// The distance between lines, as a fraction of the output height, `0.001..=0.1`.
    #[schemars(range(min = 0.001, max = 0.1))]
    pub spacing: f32,
    /// The widest a line gets, as a share of the spacing, `0..=1`, reached in the darkest tone.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub width: f32,
    /// The first layer's angle, degrees; the crossing layer runs at a right angle to it.
    pub angle_deg: f32,
    /// The ink, multiplied over the picture.
    pub ink: Colour,
    /// How much of the ink is laid, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
    /// The display luma, `0..=1`, below which the first layer starts.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub threshold: f32,
    /// The display luma, `0..=1`, below which the crossing layer starts (keep it under `threshold`; 0 draws none).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub cross_threshold: f32,
}

impl Default for Hatching {
    fn default() -> Self {
        Self {
            spacing: 0.005,
            width: 0.55,
            angle_deg: 60.0,
            ink: Colour::hex(0x001c_1626),
            strength: 0.7,
            threshold: 0.35,
            cross_threshold: 0.18,
        }
    }
}

/// A paper (or cel) surface under the picture: a soft fibre grain and a tint.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Paper {
    /// How deep the grain goes, `0..=1` of the picture's brightness.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
    /// The size of a fibre, as a fraction of the output height, `0.0005..=0.05`.
    #[schemars(range(min = 0.0005, max = 0.05))]
    pub scale: f32,
    /// The paper's colour, multiplied over the picture (white leaves it alone).
    pub tint: Colour,
}

impl Default for Paper {
    fn default() -> Self {
        Self { strength: 0.08, scale: 0.0025, tint: Colour::hex(0x00fb_f6ec) }
    }
}

/// The corners of the frame fall off, as through a lens.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Vignette {
    /// How dark the far corners get, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
    /// Where the fall-off starts, as a distance from the centre in half-heights, `0..=2` (0.8 starts just inside the
    /// top and bottom edges).
    #[schemars(range(min = 0.0, max = 2.0))]
    pub radius: f32,
    /// How long the fall-off is, in half-heights, `0.01..=2`.
    #[schemars(range(min = 0.01, max = 2.0))]
    pub softness: f32,
}

impl Default for Vignette {
    fn default() -> Self {
        Self { strength: 0.35, radius: 0.75, softness: 0.9 }
    }
}

/// Split toning: the shadows toward one colour, the highlights toward another (a teal-and-orange grade is
/// `#1f6f8b` and `#ffb070`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SplitTone {
    /// The tint the shadows take.
    pub shadows: Colour,
    /// The tint the highlights take.
    pub highlights: Colour,
    /// Where shadows end and highlights start, `-1..=1`: 0 at mid grey, positive gives the shadow tint more room.
    #[schemars(range(min = -1.0, max = 1.0))]
    pub balance: f32,
    /// How far the colour moves toward the tints, `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub strength: f32,
}

impl Default for SplitTone {
    fn default() -> Self {
        Self { shadows: Colour::hex(0x001f_6f8b), highlights: Colour::hex(0x00ff_b070), balance: 0.0, strength: 0.25 }
    }
}

/// The drawn look of the frame (see the [module docs](self)). Every part is optional; the empty value draws nothing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Stylize {
    /// Chromatic aberration at the frame's corners, as a fraction of the output height, `0..=0.02` (0 is off; 0.002 is
    /// a light lens fringe at 1080p, about 2 px).
    #[schemars(range(min = 0.0, max = 0.02))]
    pub fringe: f32,
    /// Flat tone steps, `0` (off) or `2..=64`.
    #[schemars(range(min = 0, max = 64))]
    pub posterize: u32,
    /// Split toning, if wanted.
    pub split_tone: Option<SplitTone>,
    /// A halftone dot screen, if wanted.
    pub halftone: Option<Halftone>,
    /// Ink hatching, if wanted.
    pub hatching: Option<Hatching>,
    /// A paper surface, if wanted.
    pub paper: Option<Paper>,
    /// A vignette, if wanted.
    pub vignette: Option<Vignette>,
}

impl Stylize {
    /// True when it draws nothing.
    pub fn is_empty(&self) -> bool {
        self.fringe <= 0.0
            && self.posterize == 0
            && self.split_tone.is_none_or(|s| s.strength <= 0.0)
            && self.halftone.is_none_or(|h| h.strength <= 0.0)
            && self.hatching.is_none_or(|h| h.strength <= 0.0)
            && self.paper.is_none_or(|p| p.strength <= 0.0 && p.tint == Colour::WHITE)
            && self.vignette.is_none_or(|v| v.strength <= 0.0)
    }

    /// Checks every number against its documented range, finite included, naming the field (`stylize.<field>`).
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for the first field out of range.
    pub fn validate(&self) -> Result<(), RenderError> {
        let check = |ok: bool, field: &str, range: &str| {
            if ok {
                Ok(())
            } else {
                Err(RenderError::invalid("post", format!("stylize.{field} must be a finite number {range}")))
            }
        };
        let within = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        check(within(self.fringe, 0.0, 0.02), "fringe", "in 0..=0.02")?;
        if !(self.posterize == 0 || (2..=MAX_POSTERIZE).contains(&self.posterize)) {
            return Err(RenderError::invalid("post", "stylize.posterize must be 0 (off) or 2..=64"));
        }
        if let Some(s) = self.split_tone {
            check(within(s.balance, -1.0, 1.0), "split_tone.balance", "in -1..=1")?;
            check(within(s.strength, 0.0, 1.0), "split_tone.strength", "in 0..=1")?;
        }
        if let Some(h) = self.halftone {
            check(within(h.cell, 0.001, 0.1), "halftone.cell", "in 0.001..=0.1")?;
            check(h.angle_deg.is_finite(), "halftone.angle_deg", "of degrees")?;
            check(within(h.strength, 0.0, 1.0), "halftone.strength", "in 0..=1")?;
            check(within(h.threshold, 0.0, 1.0), "halftone.threshold", "in 0..=1")?;
        }
        if let Some(h) = self.hatching {
            check(within(h.spacing, 0.001, 0.1), "hatching.spacing", "in 0.001..=0.1")?;
            check(within(h.width, 0.0, 1.0), "hatching.width", "in 0..=1")?;
            check(h.angle_deg.is_finite(), "hatching.angle_deg", "of degrees")?;
            check(within(h.strength, 0.0, 1.0), "hatching.strength", "in 0..=1")?;
            check(within(h.threshold, 0.0, 1.0), "hatching.threshold", "in 0..=1")?;
            check(within(h.cross_threshold, 0.0, 1.0), "hatching.cross_threshold", "in 0..=1")?;
        }
        if let Some(p) = self.paper {
            check(within(p.strength, 0.0, 1.0), "paper.strength", "in 0..=1")?;
            check(within(p.scale, 0.0005, 0.05), "paper.scale", "in 0.0005..=0.05")?;
        }
        if let Some(v) = self.vignette {
            check(within(v.strength, 0.0, 1.0), "vignette.strength", "in 0..=1")?;
            check(within(v.radius, 0.0, 2.0), "vignette.radius", "in 0..=2")?;
            check(within(v.softness, 0.01, 2.0), "vignette.softness", "in 0.01..=2")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_look_draws_nothing_and_every_default_is_valid() {
        assert!(Stylize::default().is_empty());
        let full = Stylize {
            fringe: 0.002,
            posterize: 4,
            split_tone: Some(SplitTone::default()),
            halftone: Some(Halftone::default()),
            hatching: Some(Hatching::default()),
            paper: Some(Paper::default()),
            vignette: Some(Vignette::default()),
        };
        assert!(!full.is_empty());
        full.validate().unwrap();
    }

    #[test]
    fn out_of_range_numbers_are_refused_by_name() {
        let bad = |s: Stylize, field: &str| {
            let e = s.validate().unwrap_err().to_string();
            assert!(e.contains(field), "{e} should name {field}");
        };
        bad(Stylize { posterize: 1, ..Stylize::default() }, "posterize");
        bad(Stylize { fringe: f32::NAN, ..Stylize::default() }, "fringe");
        bad(Stylize { halftone: Some(Halftone { cell: 0.0, ..Halftone::default() }), ..Stylize::default() }, "cell");
        bad(
            Stylize { hatching: Some(Hatching { strength: 2.0, ..Hatching::default() }), ..Stylize::default() },
            "hatching.strength",
        );
        bad(
            Stylize { vignette: Some(Vignette { softness: 0.0, ..Vignette::default() }), ..Stylize::default() },
            "vignette.softness",
        );
    }

    #[test]
    fn a_part_takes_its_defaults_for_what_it_leaves_out_and_refuses_typos() {
        let look: Stylize = serde_json::from_str(r#"{"halftone": {"cell": 0.01}}"#).unwrap();
        assert_eq!(look.halftone, Some(Halftone { cell: 0.01, ..Halftone::default() }));
        assert!(serde_json::from_str::<Stylize>(r#"{"halftone": {"cel": 0.01}}"#).is_err());
        assert!(serde_json::from_str::<Stylize>(r#"{"halftones": {}}"#).is_err());
    }

    #[test]
    fn a_look_round_trips_through_json_with_parts_left_out() {
        let look: Stylize = serde_json::from_str(
            r##"{"posterize": 3, "halftone": {"cell": 0.01, "angle_deg": 15,
            "ink": "#000000", "strength": 1, "threshold": 0.5}}"##,
        )
        .unwrap();
        assert_eq!(look.posterize, 3);
        assert_eq!(look.halftone.map(|h| h.cell), Some(0.01));
        assert!(look.hatching.is_none());
        let back: Stylize = serde_json::from_value(serde_json::to_value(look).unwrap()).unwrap();
        assert_eq!(back, look);
    }
}
