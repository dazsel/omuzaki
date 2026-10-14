//! The degrade ladder: the order in which [`Quality`] knobs are turned down (`docs/design/render-api.md`, "Degrade
//! ladder").
//!
//! Every rung only ever *caps* a knob, so a scheduler never raises anything above what the user asked for, and a rung
//! that would change nothing (the request is already cheaper than the cap) is skipped.

use omuzaki_render_api::{Quality, ShadowQuality};

/// One rung of the ladder: a single knob moved one notch down.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    /// Bloom, depth of field, grain and grade off. They are one switch today (`Quality::post_effects`; one per
    /// effect is proposed); depth of field is the most expensive, so this rung comes first.
    PostEffectsOff,
    /// Render at [`Step::SCALE_HIGH`] of the target's size.
    RenderScaleHigh,
    /// Redraw the shadow map only every [`Step::SHADOW_INTERVAL`]-th frame.
    ShadowsLessOften,
    /// Small shadow map ([`ShadowQuality::Low`]).
    ShadowsLow,
    /// Render at [`Step::SCALE_LOW`] of the target's size.
    RenderScaleLow,
    /// No shadow pass at all ([`ShadowQuality::Off`]).
    ShadowsOff,
    /// At most two samples per pixel.
    SamplesTwo,
    /// One sample per pixel.
    SamplesOne,
}

impl Step {
    /// Every rung, cheapest look-loss first. The costs behind this order are estimates until real measurements are
    /// wired in; the order is one table, so re-ordering is a one-line change.
    pub const LADDER: [Self; 8] = [
        Self::PostEffectsOff,
        Self::RenderScaleHigh,
        Self::ShadowsLessOften,
        Self::ShadowsLow,
        Self::RenderScaleLow,
        Self::ShadowsOff,
        Self::SamplesTwo,
        Self::SamplesOne,
    ];
    /// `shadow_interval` after [`Step::ShadowsLessOften`].
    pub const SHADOW_INTERVAL: u8 = 4;
    /// `render_scale` cap after [`Step::RenderScaleHigh`].
    pub const SCALE_HIGH: f32 = 0.75;
    /// `render_scale` cap after [`Step::RenderScaleLow`].
    pub const SCALE_LOW: f32 = 0.5;

    /// A stable lower-case name for logs and the "quality was lowered" notice.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PostEffectsOff => "post_effects_off",
            Self::ShadowsLessOften => "shadows_less_often",
            Self::RenderScaleHigh => "render_scale_high",
            Self::ShadowsLow => "shadows_low",
            Self::RenderScaleLow => "render_scale_low",
            Self::ShadowsOff => "shadows_off",
            Self::SamplesTwo => "samples_two",
            Self::SamplesOne => "samples_one",
        }
    }

    /// Applies the rung to `quality`.
    pub(crate) fn apply(self, quality: &mut Quality) {
        match self {
            Self::PostEffectsOff => quality.post_effects = false,
            Self::ShadowsLessOften => quality.shadow_interval = quality.shadow_interval.max(Self::SHADOW_INTERVAL),
            Self::RenderScaleHigh => quality.render_scale = quality.render_scale.min(Self::SCALE_HIGH),
            Self::ShadowsLow => {
                if quality.shadows == ShadowQuality::High {
                    quality.shadows = ShadowQuality::Low;
                }
            }
            Self::RenderScaleLow => quality.render_scale = quality.render_scale.min(Self::SCALE_LOW),
            Self::ShadowsOff => quality.shadows = ShadowQuality::Off,
            Self::SamplesTwo => quality.samples = quality.samples.min(2),
            Self::SamplesOne => quality.samples = 1,
        }
    }
}

/// `requested` with the first `rungs` rungs of the ladder applied.
pub(crate) fn quality_at(requested: &Quality, rungs: usize) -> Quality {
    let mut quality = *requested;
    for step in Step::LADDER.iter().take(rungs) {
        step.apply(&mut quality);
    }
    quality
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_walks_high_down_to_the_floor() {
        let floor = quality_at(&Quality::high(), Step::LADDER.len());
        assert!(!floor.post_effects);
        assert_eq!((floor.shadows, floor.shadow_interval, floor.samples), (ShadowQuality::Off, 4, 1));
        assert!((floor.render_scale - Step::SCALE_LOW).abs() < f32::EPSILON);
        assert_eq!(floor.validate(), Ok(()), "every rung leaves a valid quality");
    }

    #[test]
    fn rungs_only_lower_never_raise() {
        let mut cheap = Quality::live();
        cheap.render_scale = 0.4;
        cheap.shadow_interval = 8;
        let floor = quality_at(&cheap, Step::LADDER.len());
        assert!((floor.render_scale - 0.4).abs() < f32::EPSILON, "already below the caps");
        assert_eq!(floor.shadow_interval, 8);
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = Step::LADDER.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Step::LADDER.len());
    }
}
