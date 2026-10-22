//! The player source (BR-21): decoded sounds the engine holds in a bank, played by players into the buses that name
//! them as `SourceRef::Player`, for the soundboard and music.
//!
//! - A [`Sound`] is planar 48 kHz PCM, one or two channels, shared as an `Arc` so the audio thread never copies it.
//!   An unloaded sound goes back from the audio thread to the control side to be freed, never on the audio thread.
//! - A player plays up to [`VOICES`] sounds at once. One more steals the oldest, which fades out over
//!   [`VOICE_FADE`] samples; stopping a sound fades it out the same way.
//! - **Duck gates:** a player raises the gate with its own name ([`gate_of`]) while any of its voices that `ducks`
//!   is sounding, fade-out included, so a duck rule triggered by `DuckTrigger::Gate` lowers a bus under the sound.
//! - A mono sound reaches the player's stereo output equal-power: `x / sqrt(2)` on each side, as the mixer's
//!   mono-to-stereo rule.

use crate::engine::AudioError;
use crate::ids::{GateId, PlayerId};
use crate::node::SAMPLE_RATE;

/// The most voices one player sounds at once; one more steals the oldest.
pub const VOICES: usize = 16;

/// A stopped or stolen voice's fade-out, in samples: 5 ms.
pub const VOICE_FADE: u32 = 240;

/// The longest sound the bank takes, in seconds (longer music plays through a feed).
pub const MAX_SOUND_SECONDS: u32 = 600;

/// The gate a player raises while a sound that ducks is sounding: the gate with the player's own name.
#[must_use]
pub fn gate_of(player: &PlayerId) -> GateId {
    GateId(player.0.clone())
}

/// A decoded sound: planar 48 kHz PCM, one or two channels of equal length.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    channels: Vec<Vec<f32>>,
}

impl Sound {
    /// A sound from its channels.
    ///
    /// # Errors
    /// [`AudioError::Internal`] (the caller's bug) for no channel, more than two, channels of different lengths, or
    /// more than [`MAX_SOUND_SECONDS`] of audio.
    pub fn new(channels: Vec<Vec<f32>>) -> Result<Self, AudioError> {
        let frames = channels.first().map_or(0, Vec::len);
        if channels.is_empty() || channels.len() > 2 {
            return Err(AudioError::Internal(format!("a sound has 1 or 2 channels, not {}", channels.len())));
        }
        if channels.iter().any(|c| c.len() != frames) {
            return Err(AudioError::Internal("a sound's channels must be the same length".to_owned()));
        }
        let limit = usize::try_from(MAX_SOUND_SECONDS * SAMPLE_RATE).unwrap_or(usize::MAX);
        if frames > limit {
            return Err(AudioError::Internal(format!("a sound is at most {MAX_SOUND_SECONDS} s; stream longer music")));
        }
        Ok(Self { channels })
    }

    /// 1 or 2.
    #[must_use]
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Frames per channel.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// Channel `c` (the first for a mono sound asked for its second).
    #[must_use]
    pub fn channel(&self, c: usize) -> &[f32] {
        self.channels.get(c).or_else(|| self.channels.first()).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_is_one_or_two_equal_channels() {
        assert!(Sound::new(vec![]).is_err());
        assert!(Sound::new(vec![vec![0.0; 4]; 3]).is_err());
        assert!(Sound::new(vec![vec![0.0; 4], vec![0.0; 5]]).is_err());
        let mono = Sound::new(vec![vec![0.5; 4]]).unwrap();
        assert_eq!((mono.channel_count(), mono.frames()), (1, 4));
        assert_eq!(mono.channel(1), mono.channel(0));
        assert_eq!(gate_of(&PlayerId::new("board").unwrap()).as_str(), "board");
    }
}
