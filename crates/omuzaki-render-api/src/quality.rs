//! Quality: how much work a frame may spend, separate from what the frame *looks like* (the [`Post`](crate::Post)
//! look is the author's; quality is the budget).
//!
//! Keeping the two apart is what lets the frame-budget scheduler (spec §8) degrade gracefully: it turns individual
//! knobs down (shadow updates, resolution, post) without ever touching the author's lights, materials or grade.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RenderError;
use crate::limits::MAX_SAMPLES;

/// The GPU time budgets of spec §10 (mid laptop, 1080p30, one character, face and upper body).
pub mod budget {
    use super::Seconds;

    /// Whole-frame GPU budget in [`QualityMode::Live`](crate::QualityMode::Live) with the live preset.
    pub const LIVE_GPU: Seconds = Seconds(0.0015);
    /// Whole-frame GPU budget with the high preset.
    pub const HIGH_GPU: Seconds = Seconds(0.004);
    /// GPU budget of the face painter, reported under [`Pass::PaintTextures`](crate::Pass::PaintTextures). §10 lists it
    /// as its own line, so it is *not* part of [`LIVE_GPU`].
    pub const FACE_PAINT_GPU: Seconds = Seconds(0.0005);
}

/// Whether the renderer may take shortcuts that depend on earlier frames.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QualityMode {
    /// Real time (streaming, the editor viewport). The renderer may reuse work from earlier frames, such as a shadow
    /// map redrawn every `shadow_interval`-th frame, and may drop effects when the scheduler asks it to.
    Live,
    /// Frame-exact (SH-05 renders, goldens, thumbnails). The output depends only on the resources, the scene and the
    /// quality settings, never on frames drawn before it or on timing: shadow maps are redrawn every frame and no
    /// temporal accumulation is used. Time is not a constraint.
    Offline,
}

/// Shadow-map quality.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadowQuality {
    /// No shadow pass at all.
    Off,
    /// A small map with hard-ish filtering: enough to keep hair and chin shading (the live default).
    Low,
    /// A large map with soft filtering.
    High,
}

impl ShadowQuality {
    /// How many shadow-casting directional lights get a map at this quality: none when off, the first one at `Low`
    /// (the live default keeps one map's cost), up to [`MAX_SHADOW_MAPS`](crate::limits::MAX_SHADOW_MAPS) at `High`.
    pub fn max_maps(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Low => 1,
            Self::High => crate::limits::MAX_SHADOW_MAPS,
        }
    }
}

/// The knobs that trade image quality for GPU time. Presets: [`Quality::live`], [`Quality::high`], [`Quality::offline`].
///
/// The preset numbers are starting points; the toon-viewport spike tunes them against the §10 budgets.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Quality {
    /// Live or frame-exact.
    pub mode: QualityMode,
    /// Internal resolution as a multiple of the target's: below 1 renders smaller and scales up, above 1 supersamples.
    /// `0.25..=4.0`.
    pub render_scale: f32,
    /// Samples per pixel for edges: a power of two up to [`MAX_SAMPLES`]. Hardware MSAA covers the first few; a
    /// renderer reaches the rest by jittered supersampling on a fixed pattern, so the result is still deterministic.
    pub samples: u8,
    /// Shadow-map quality.
    pub shadows: ShadowQuality,
    /// In `Live` mode, redraw shadow maps every this-many frames. Ignored in `Offline`.
    ///
    /// The valid range is `1..=255`: `1` redraws every frame, and `255` redraws about every 4.25 s at 60 fps. A moving
    /// caster's shadow may therefore lag by up to that long. A turned light redraws at once whatever the interval. `0` is
    /// rejected by [`Quality::validate`] rather than clamped, because a silent clamp would hide a mistake in the caller's
    /// settings.
    pub shadow_interval: u8,
    /// Whether the scene's bloom, depth of field, grain and grade run at all. Exposure and tone mapping always run.
    pub post_effects: bool,
}

impl Quality {
    /// The smallest `render_scale`.
    pub const MIN_SCALE: f32 = 0.25;
    /// The largest `render_scale`.
    pub const MAX_SCALE: f32 = 4.0;

    /// Streaming and the editor viewport: cheap, to stay inside [`budget::LIVE_GPU`].
    pub fn live() -> Self {
        Self {
            mode: QualityMode::Live,
            render_scale: 1.0,
            samples: 4,
            shadows: ShadowQuality::Low,
            shadow_interval: 2,
            post_effects: false,
        }
    }

    /// The editor's "quality mode" (LK-05): live, but with the full look; inside [`budget::HIGH_GPU`].
    pub fn high() -> Self {
        Self {
            mode: QualityMode::Live,
            render_scale: 1.5,
            samples: 4,
            shadows: ShadowQuality::High,
            shadow_interval: 1,
            post_effects: true,
        }
    }

    /// Final renders: the most the renderer can do, frame-exact.
    pub fn offline() -> Self {
        Self {
            mode: QualityMode::Offline,
            render_scale: 1.0,
            samples: MAX_SAMPLES,
            shadows: ShadowQuality::High,
            shadow_interval: 1,
            post_effects: true,
        }
    }

    /// Checks the ranges.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] for a scale outside `0.25..=4`, samples that aren't a power of two up to 16, or a
    /// zero shadow interval.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: String| Err(RenderError::invalid("quality", reason));
        if !(Self::MIN_SCALE..=Self::MAX_SCALE).contains(&self.render_scale) {
            return bad(format!(
                "render_scale {} isn't in {}..={}",
                self.render_scale,
                Self::MIN_SCALE,
                Self::MAX_SCALE
            ));
        }
        if !self.samples.is_power_of_two() || self.samples > MAX_SAMPLES {
            return bad(format!("samples {} isn't a power of two up to {MAX_SAMPLES}", self.samples));
        }
        if self.shadow_interval == 0 {
            return bad("shadow_interval must be at least 1".into());
        }
        Ok(())
    }
}

impl Default for Quality {
    fn default() -> Self {
        Self::live()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Break = fn(&mut Quality);

    #[test]
    fn presets_are_valid_and_ordered_by_cost() {
        for q in [Quality::live(), Quality::high(), Quality::offline()] {
            assert_eq!(q.validate(), Ok(()));
        }
        assert_eq!(Quality::default(), Quality::live());
        assert_eq!(Quality::live().mode, QualityMode::Live);
        assert_eq!(Quality::offline().mode, QualityMode::Offline);
        assert!(Quality::live().render_scale <= Quality::high().render_scale);
        assert!(!Quality::live().post_effects && Quality::offline().post_effects);
    }

    #[test]
    fn invalid_settings_say_what_to_fix() {
        let cases: [(&str, Break); 4] = [
            ("render_scale", |q| q.render_scale = 8.0),
            ("render_scale", |q| q.render_scale = f32::NAN),
            ("samples", |q| q.samples = 3),
            ("shadow_interval", |q| q.shadow_interval = 0),
        ];
        for (needle, break_it) in cases {
            let mut q = Quality::live();
            break_it(&mut q);
            let err = q.validate().unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
        let mut q = Quality::live();
        q.samples = 32;
        assert!(q.validate().is_err(), "more than MAX_SAMPLES");
    }

    #[test]
    fn shadow_interval_takes_one_to_255_and_says_so_when_zero() {
        for interval in [1, 2, 255] {
            let mut q = Quality::live();
            q.shadow_interval = interval;
            assert_eq!(q.validate(), Ok(()), "interval {interval} is in 1..=255");
        }
        let mut q = Quality::live();
        q.shadow_interval = 0;
        let err = q.validate().unwrap_err().to_string();
        assert!(err.contains("shadow_interval must be at least 1"), "{err:?}");
    }

    #[test]
    fn budgets_match_the_spec_table() {
        assert_eq!((budget::LIVE_GPU, budget::HIGH_GPU), (Seconds(0.0015), Seconds(0.004)));
        assert!(budget::FACE_PAINT_GPU < budget::LIVE_GPU);
    }

    #[test]
    fn quality_round_trips_through_json() {
        let q = Quality::high();
        let json = serde_json::to_value(q).unwrap();
        assert_eq!(json["mode"], "live");
        assert_eq!(serde_json::from_value::<Quality>(json).unwrap(), q);
    }
}
