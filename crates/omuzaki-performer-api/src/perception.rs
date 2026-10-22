//! Opt-in sight: the [`Perception`] seam and a [`Glance`] (`ai-performer.md` §5.7). Off by default and per source;
//! a glance is attached to a turn and never stored.

use std::fmt;

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::limits::GLANCE_MAX_PX;

/// One source the person picked: a window or screen, or a camera (local models only, SE-04). The App implements it
/// over the capture it already has.
pub trait Perception: Send {
    /// The latest frame as a glance, or `None` when there is nothing new to see.
    ///
    /// # Errors
    /// [`PerceptionError`] when the source is gone or the frame cannot be encoded.
    fn glance(&mut self) -> Result<Option<Glance>, PerceptionError>;
}

/// What the performer looks at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GlanceSource {
    /// A window or screen, named as the consent card shows it.
    Screen(String),
    /// A camera; its frames reach local vision models only.
    Camera(String),
}

/// One frame, JPEG, at most [`GLANCE_MAX_PX`] on its long side.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct Glance {
    /// Where it came from.
    pub source: GlanceSource,
    /// When it was taken, on the runner clock.
    pub at: Seconds,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The JPEG bytes.
    pub jpeg: Vec<u8>,
}

impl Glance {
    /// A glance of `width` × `height` pixels.
    ///
    /// # Errors
    /// [`PerceptionError::TooLarge`] when the long side is over [`GLANCE_MAX_PX`], [`PerceptionError::NotJpeg`]
    /// when the bytes don't start with a JPEG marker.
    pub fn new(
        source: GlanceSource,
        at: Seconds,
        width: u32,
        height: u32,
        jpeg: Vec<u8>,
    ) -> Result<Self, PerceptionError> {
        let long = width.max(height);
        if long > GLANCE_MAX_PX {
            return Err(PerceptionError::too_large(long));
        }
        if !jpeg.starts_with(&[0xFF, 0xD8]) {
            return Err(PerceptionError::NotJpeg);
        }
        Ok(Self { source, at, width, height, jpeg })
    }
}

impl fmt::Debug for Glance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Glance")
            .field("source", &self.source)
            .field("at", &self.at)
            .field("size", &(self.width, self.height))
            .field("jpeg_bytes", &self.jpeg.len())
            .finish()
    }
}

/// Why there is no glance.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PerceptionError {
    /// The window, screen or camera went away.
    #[error("the source is gone: {0}")]
    SourceGone(String),
    /// The frame is larger than a glance may be.
    #[error("a glance is at most {max} px on its long side (got {long})")]
    #[non_exhaustive]
    TooLarge {
        /// The long side given.
        long: u32,
        /// [`GLANCE_MAX_PX`].
        max: u32,
    },
    /// The bytes are not a JPEG.
    #[error("a glance must be a JPEG")]
    NotJpeg,
}

impl PerceptionError {
    /// [`PerceptionError::TooLarge`] for a frame whose long side is `long` pixels.
    pub fn too_large(long: u32) -> Self {
        Self::TooLarge { long, max: GLANCE_MAX_PX }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glances_are_small_jpegs() {
        let src = || GlanceSource::Screen("Game window".into());
        let ok = Glance::new(src(), Seconds(1.0), 768, 432, vec![0xFF, 0xD8, 0xFF]).unwrap();
        assert!(format!("{ok:?}").contains("jpeg_bytes: 3"));
        assert_eq!(
            Glance::new(src(), Seconds(1.0), 769, 10, vec![0xFF, 0xD8]),
            Err(PerceptionError::TooLarge { long: 769, max: GLANCE_MAX_PX })
        );
        assert_eq!(Glance::new(src(), Seconds(1.0), 10, 10, vec![0x89, b'P']), Err(PerceptionError::NotJpeg));
    }
}
