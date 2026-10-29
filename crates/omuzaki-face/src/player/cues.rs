//! A short's expression cues (#2409): a timeline of shows, cuts and hides played through an [`ExpressionPlayer`], so a
//! rendered short honours each expression's playback exactly as the live player does.
//!
//! The cues are applied in time order (cues at the same time in the order given), each at its own time: the fades run
//! up to a cue, the cue is applied, and the fades run on. [`CueTrack::advance_to`] takes any frame time, so a cue lands
//! on the frame at or after it whatever the frame rate, and two runs of the same cues give the same faces.

use omuzaki_anim_api::ChannelBuf;
use omuzaki_face_api::{ExpressionId, ExpressionSet, UnknownExpression};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExpressionPlayer, MAX_DT};

/// What a cue does to its expression.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CueAction {
    /// Shows it as its playback says (fades, group, layer, auto-stop).
    #[default]
    Show,
    /// Cuts to it on the frame ([`ExpressionPlayer::cut`]).
    Cut,
    /// Fades it out.
    Hide,
}

/// One cue: at `at_s` seconds into the short, `action` on `expression`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpressionCue {
    /// Seconds from the start of the short, `0` or more.
    pub at_s: f64,
    /// The expression.
    pub expression: ExpressionId,
    /// What happens to it. Default: show.
    #[serde(default)]
    pub action: CueAction,
}

/// Why a cue track could not be made or played.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CueError {
    /// A cue's time is not a finite number of seconds from 0.
    #[error("cue {index} is at {at_s} s: a cue's time must be a number of seconds from 0")]
    BadTime {
        /// The cue's place in the list given.
        index: usize,
        /// Its time.
        at_s: f64,
    },
    /// A cue names an expression the character doesn't have.
    #[error(transparent)]
    Unknown(#[from] UnknownExpression),
}

/// A short's cues and the player they drive. See the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct CueTrack {
    cues: Vec<ExpressionCue>,
    /// The next cue to apply.
    next: usize,
    /// The time the player has been stepped to.
    now: f64,
    player: ExpressionPlayer,
}

impl CueTrack {
    /// The cues sorted by time (ties in the order given), checked against `set`, with the player returning to
    /// `default` where nothing held shows.
    ///
    /// # Errors
    /// [`CueError::BadTime`] for a negative or non-finite time, [`CueError::Unknown`] for an expression `set` lacks.
    pub fn new(
        set: &ExpressionSet,
        default: Option<ExpressionId>,
        mut cues: Vec<ExpressionCue>,
    ) -> Result<Self, CueError> {
        for (index, cue) in cues.iter().enumerate() {
            if !cue.at_s.is_finite() || cue.at_s < 0.0 {
                return Err(CueError::BadTime { index, at_s: cue.at_s });
            }
            if !set.0.contains_key(&cue.expression) {
                return Err(UnknownExpression(cue.expression.clone()).into());
            }
        }
        cues.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));
        Ok(Self { cues, next: 0, now: 0.0, player: ExpressionPlayer::new(default) })
    }

    /// Plays every cue up to and including `t_s` and runs the fades to `t_s`. A time before the last one asked for
    /// changes nothing (a short plays forward; make a new track to start over).
    ///
    /// # Errors
    /// [`CueError::Unknown`] when a cue's expression has left `set` since the track was made; the cues before it are
    /// applied.
    pub fn advance_to(&mut self, set: &ExpressionSet, t_s: f64) -> Result<(), CueError> {
        if !t_s.is_finite() || t_s < self.now {
            return Ok(());
        }
        while let Some(cue) = self.cues.get(self.next).filter(|cue| cue.at_s <= t_s) {
            let (at, expression, action) = (cue.at_s, cue.expression.clone(), cue.action);
            self.run(at);
            self.next += 1;
            match action {
                CueAction::Show => self.player.show(set, &expression)?,
                CueAction::Cut => self.player.cut(set, &expression)?,
                CueAction::Hide => self.player.hide(&expression),
            }
        }
        self.run(t_s);
        Ok(())
    }

    /// `base` with what shows now ([`ExpressionPlayer::blend`]).
    pub fn blend(&self, set: &ExpressionSet, base: &ChannelBuf) -> ChannelBuf {
        self.player.blend(set, base)
    }

    /// The player, to read weights.
    pub fn player(&self) -> &ExpressionPlayer {
        &self.player
    }

    /// Steps the fades from `now` to `to`, in steps the player takes whole.
    #[allow(clippy::cast_possible_truncation)] // a step of at most a quarter second fits f32
    fn run(&mut self, to: f64) {
        while to > self.now {
            let left = to - self.now;
            let dt = left.min(f64::from(MAX_DT));
            self.player.step(dt as f32);
            self.now = if dt >= left { to } else { self.now + dt };
        }
    }
}
