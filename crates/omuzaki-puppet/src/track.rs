//! A puppet's performance as a track: per frame, the view (puppet to picture) and the parameters that change. It is how
//! a performance recorded elsewhere (a solved video, a tracker's log, an agent's keyframes) plays back on a puppet
//! frame for frame, and how the reference loop re-renders a reference it has solved.
//!
//! A track is plain JSON, so a script writes one as easily as the studio does:
//!
//! ```json
//! { "size": [540, 960], "fps": 30, "background": [0, 0, 0, 255],
//!   "frames": [ { "view": [0.5, 0, 0, 0.5, 270, 480], "params": { "ParamAngleX": 10, "Expr smile": 1 } },
//!               { "params": { "ParamAngleX": 12 } } ] }
//! ```
//!
//! A frame without a `view` keeps the one before; parameters keep their values until a frame sets them again.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Affine2, Mat2, Vec2};
use omuzaki_puppet_api::ParamIx;
use serde::{Deserialize, Serialize};

use crate::eval::Evaluator;
use crate::physics::Physics;
use crate::raster::{Canvas, CpuRenderer};
use crate::texture::Texture;

/// The most frames a track holds: an hour at 60 frames a second.
pub const MAX_FRAMES: usize = 216_000;
/// The largest picture side.
pub const MAX_SIDE: u32 = 8192;

/// A parameter's value in a track: one number for a one-axis parameter, two for a two-axis one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TrackValue {
    /// The x value.
    One(f32),
    /// The x and y values.
    Two([f32; 2]),
}

impl TrackValue {
    fn pair(self) -> [f32; 2] {
        match self {
            Self::One(x) => [x, 0.0],
            Self::Two(xy) => xy,
        }
    }
}

/// One frame of a track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackFrame {
    /// Puppet (model space) to picture pixels, `[a, b, c, d, e, f]`: `x' = a x + c y + e`, `y' = b x + d y + f`. The
    /// frame before's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<[f32; 6]>,
    /// Parameters that change at this frame, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, TrackValue>,
}

fn default_fps() -> u32 {
    30
}

fn default_background() -> [u8; 4] {
    [0, 0, 0, 0]
}

/// A track: the picture's size and rate, its backdrop, and the frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PuppetTrack {
    /// Width and height in pixels.
    pub size: [u32; 2],
    /// Frames per second (the pendulums step at it).
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// The backdrop, straight sRGB RGBA8; transparent by default.
    #[serde(default = "default_background")]
    pub background: [u8; 4],
    /// Whether the puppet's pendulums swing; when false, the track sets every parameter itself.
    #[serde(default)]
    pub physics: bool,
    /// The frames.
    pub frames: Vec<TrackFrame>,
}

/// Why a track didn't play.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrackError {
    /// The track breaks a rule.
    #[error("track: {0}")]
    Invalid(String),
    /// The frame sink failed.
    #[error("track frame {frame}: {detail}")]
    Sink {
        /// The frame.
        frame: usize,
        /// What the sink said.
        detail: String,
    },
}

/// The affine of a track's `view`.
#[must_use]
pub fn view_affine(v: [f32; 6]) -> Affine2 {
    Affine2::from_mat2_translation(Mat2::from_cols(Vec2::new(v[0], v[1]), Vec2::new(v[2], v[3])), Vec2::new(v[4], v[5]))
}

impl PuppetTrack {
    /// Checks the sizes and that every parameter it names is the puppet's.
    ///
    /// # Errors
    /// [`TrackError::Invalid`] naming the first problem.
    pub fn check(&self, eval: &Evaluator) -> Result<(), TrackError> {
        let [w, h] = self.size;
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(TrackError::Invalid(format!("a picture is 1 to {MAX_SIDE} pixels a side, not {w}x{h}")));
        }
        if !(1..=240).contains(&self.fps) {
            return Err(TrackError::Invalid(format!("{} frames a second is outside 1 to 240", self.fps)));
        }
        if self.frames.len() > MAX_FRAMES {
            return Err(TrackError::Invalid(format!("at most {MAX_FRAMES} frames")));
        }
        for (i, frame) in self.frames.iter().enumerate() {
            if frame.view.is_some_and(|v| v.iter().any(|x| !x.is_finite())) {
                return Err(TrackError::Invalid(format!("frame {i}: the view isn't finite")));
            }
            for (name, value) in &frame.params {
                if eval.def().param(name).is_none() {
                    return Err(TrackError::Invalid(format!("frame {i}: the puppet has no parameter {name:?}")));
                }
                if value.pair().iter().any(|x| !x.is_finite()) {
                    return Err(TrackError::Invalid(format!("frame {i}: {name} isn't finite")));
                }
            }
        }
        Ok(())
    }

    /// Plays the track on `eval` (drawn with `textures`), handing each frame's straight RGBA8 pixels to `sink` in
    /// order. Parameters the track never sets stay at their defaults, or swing with the pendulums when `physics` is on.
    ///
    /// # Errors
    /// [`TrackError::Invalid`] for a track that fails [`PuppetTrack::check`]; [`TrackError::Sink`] when `sink` fails.
    pub fn play(
        &self,
        eval: &mut Evaluator,
        textures: &[Texture],
        renderer: &mut CpuRenderer,
        mut sink: impl FnMut(usize, &[u8]) -> Result<(), String>,
    ) -> Result<(), TrackError> {
        self.check(eval)?;
        let [w, h] = self.size;
        let mut canvas = Canvas::new(w, h);
        let a = f32::from(self.background[3]) / 255.0;
        let fill = [
            f32::from(self.background[0]) / 255.0 * a,
            f32::from(self.background[1]) / 255.0 * a,
            f32::from(self.background[2]) / 255.0 * a,
            a,
        ];
        let mut physics = Physics::new(eval.def().physics.len());
        #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 240")]
        let dt = 1.0 / self.fps as f32;
        let mut view = Affine2::IDENTITY;
        let mut held: Vec<(ParamIx, [f32; 2])> = Vec::new();
        for (i, frame) in self.frames.iter().enumerate() {
            if let Some(v) = frame.view {
                view = view_affine(v);
            }
            for (name, value) in &frame.params {
                if let Some(ix) = eval.def().param(name) {
                    let pair = value.pair();
                    match held.iter_mut().find(|(p, _)| *p == ix) {
                        Some(slot) => slot.1 = pair,
                        None => held.push((ix, pair)),
                    }
                }
            }
            for (ix, pair) in &held {
                eval.set(*ix, *pair);
            }
            if self.physics {
                physics.step(eval, dt);
            }
            eval.evaluate();
            canvas.fill(fill);
            renderer.draw(eval.draw_list(), textures, view, &mut canvas);
            sink(i, &canvas.to_straight_rgba8()).map_err(|detail| TrackError::Sink { frame: i, detail })?;
        }
        Ok(())
    }
}
