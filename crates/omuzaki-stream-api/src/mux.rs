//! Muxer traits.

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::encoder::StreamSpec;
use crate::error::StreamError;
use crate::packet::{Packet, StreamId};

/// A container format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Container {
    /// `QuickTime` (`ProRes`).
    Mov,
    /// `WebM` (VP9 with alpha).
    Webm,
    /// MP4; fragmented when written live (D6).
    Mp4,
    /// Matroska.
    Mkv,
}

/// What a finished file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MuxReport {
    /// Packets written, all streams.
    pub packets: u64,
    /// Bytes in the finished file.
    pub bytes: u64,
}

impl MuxReport {
    /// A report of this many packets and bytes.
    pub fn new(packets: u64, bytes: u64) -> Self {
        Self { packets, bytes }
    }
}

/// One output file being written.
pub trait Muxer: Send {
    /// Adds a stream before the first packet and returns the id its packets must carry.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the container cannot hold the stream; [`StreamError::Mux`] if packets already
    /// started.
    fn add_stream(&mut self, spec: &StreamSpec) -> Result<StreamId, StreamError>;

    /// Writes a packet, side data included.
    ///
    /// # Errors
    /// [`StreamError::Mux`] or [`StreamError::Io`] on failure.
    fn write(&mut self, packet: &Packet) -> Result<(), StreamError>;

    /// Writes the trailer and closes the file.
    ///
    /// # Errors
    /// [`StreamError::Mux`] or [`StreamError::Io`] on failure.
    fn finish(self: Box<Self>) -> Result<MuxReport, StreamError>;
}

/// Opens muxers.
pub trait MuxerFactory: Send + Sync {
    /// Creates `path` as a `container` file.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] with a hint if the container is not supported here, [`StreamError::Io`] if the file
    /// cannot be created.
    fn create(&self, path: &Path, container: Container) -> Result<Box<dyn Muxer>, StreamError>;
}
