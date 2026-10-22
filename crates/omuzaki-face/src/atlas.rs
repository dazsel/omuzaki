//! The atlas-scale controller (#808): steps the face layers' raster density down when the face painter runs over its
//! GPU budget (`gpu.face_paint`, spec §10) and back up when there is room, with hysteresis so it never flaps.
//!
//! The stage drives it. Once a frame it maps the scheduler's face pressure (`omuzaki_budget::Scheduler::face_pressure`)
//! onto [`AtlasPressure`] and calls [`AtlasController::update`]. When that returns an [`AtlasScaleChange`], the stage
//! calls [`FaceSystem::set_layer_scale`](crate::FaceSystem::set_layer_scale), creates the new layer textures, updates
//! the material and releases the old textures, in that order (`docs/design/render-api.md`). The controller is pure: no
//! clock, no allocation, the same inputs give the same changes.
//!
//! The defaults are a first guess; they are tuned with the owner's GPU numbers (#1658).

use omuzaki_face_api::FaceError;

/// How the face painter's smoothed GPU time compares with its budget, as the controller reads it. The stage maps the
/// scheduler's face pressure onto it (no measurement yet is [`Within`](Self::Within): hold).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AtlasPressure {
    /// Over the face budget: draw fewer pixels.
    Over,
    /// Inside the budget: keep the scale.
    Within,
    /// Well under the budget: there is room for more pixels.
    Under,
}

/// A change of the layer scale, from [`AtlasController::update`]: hand `to` to
/// [`FaceSystem::set_layer_scale`](crate::FaceSystem::set_layer_scale).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AtlasScaleChange {
    /// The scale before the change.
    pub from: f32,
    /// The scale after it.
    pub to: f32,
}

/// The controller's bounds and timing. Counts are in updates (frames).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AtlasConfig {
    /// The lowest scale, above 0.
    pub min_scale: f32,
    /// The highest scale, at least `min_scale`: the layers' full density is 1.
    pub max_scale: f32,
    /// How far one change moves the scale, above 0.
    pub step: f32,
    /// How many updates in a row the same pressure must hold before the scale moves; at least 2, so a pressure that
    /// alternates never moves it.
    pub hold: u32,
    /// How many updates after a change the scale stays put, whatever the pressure: the change has to show in the
    /// smoothed time before it is judged.
    pub cooldown: u32,
}

impl Default for AtlasConfig {
    /// From full density down to half, in eighths; half a second of pressure at 60 Hz to move, two seconds between moves.
    fn default() -> Self {
        Self { min_scale: 0.5, max_scale: 1.0, step: 0.125, hold: 30, cooldown: 120 }
    }
}

impl AtlasConfig {
    /// Checks the bounds and counts.
    ///
    /// # Errors
    /// [`FaceError::Invalid`] naming the field that is out of range.
    pub fn validate(&self) -> Result<(), FaceError> {
        let invalid = |field: &str, why: &str| Err(FaceError::Invalid(format!("atlas config: {field} {why}")));
        if !(self.min_scale.is_finite() && self.min_scale > 0.0) {
            return invalid("min_scale", "must be above 0");
        }
        if !(self.max_scale.is_finite() && self.max_scale >= self.min_scale) {
            return invalid("max_scale", "must be at least min_scale");
        }
        if !(self.step.is_finite() && self.step > 0.0) {
            return invalid("step", "must be above 0");
        }
        if self.hold < 2 {
            return invalid("hold", "must be at least 2, or an alternating pressure would move the scale");
        }
        Ok(())
    }

    /// How many steps lie between the bounds (the last one may be short: it stops at `min_scale`).
    fn levels(&self) -> u32 {
        // A few steps at most; the bounds are validated finite and ordered.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let levels = ((self.max_scale - self.min_scale) / self.step).ceil() as u32;
        levels
    }
}

/// Steps the layer scale with the face pressure (see the module docs). The scale is a whole number of steps below
/// `max_scale`, so it never drifts.
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasController {
    config: AtlasConfig,
    /// Steps below `max_scale`.
    level: u32,
    /// The pressure of the current run and how many updates it has held.
    run: Option<(AtlasPressure, u32)>,
    /// Updates left before the scale may move again.
    cooldown: u32,
}

impl AtlasController {
    /// A controller at full scale (`max_scale`).
    ///
    /// # Errors
    /// As [`AtlasConfig::validate`].
    pub fn new(config: AtlasConfig) -> Result<Self, FaceError> {
        config.validate()?;
        Ok(Self { config, level: 0, run: None, cooldown: 0 })
    }

    /// The scale now: what [`FaceSystem::set_layer_scale`](crate::FaceSystem::set_layer_scale) was last given.
    pub fn scale(&self) -> f32 {
        self.scale_at(self.level)
    }

    fn scale_at(&self, level: u32) -> f32 {
        // A handful of levels: exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let below = level as f32 * self.config.step;
        (self.config.max_scale - below).max(self.config.min_scale)
    }

    /// One update with this frame's `pressure`: the change of scale to make now, if any. A pressure moves the scale
    /// one step once it has held for `hold` updates in a row and no change happened in the last `cooldown` updates;
    /// the scale never leaves its bounds.
    pub fn update(&mut self, pressure: AtlasPressure) -> Option<AtlasScaleChange> {
        let held = match self.run {
            Some((last, n)) if last == pressure => n.saturating_add(1),
            _ => 1,
        };
        self.run = Some((pressure, held));
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return None;
        }
        if held < self.config.hold {
            return None;
        }
        let level = match pressure {
            AtlasPressure::Over if self.level < self.config.levels() => self.level + 1,
            AtlasPressure::Under if self.level > 0 => self.level - 1,
            AtlasPressure::Over | AtlasPressure::Within | AtlasPressure::Under => return None,
        };
        let change = AtlasScaleChange { from: self.scale(), to: self.scale_at(level) };
        self.level = level;
        self.run = None;
        self.cooldown = self.config.cooldown;
        Some(change)
    }
}
