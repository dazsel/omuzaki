//! The `WebM` writer behind the [`Muxer`] trait, for stream copies (a saved replay clip): one VP9 video stream, its
//! alpha frames taken from `BlockAdditional` side data. Packets are timed in frames (the stream's timebase is one frame).

use std::path::{Path, PathBuf};

use omuzaki_stream_api::encoder::{StreamKind, StreamSpec};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::mux::{Container, MuxReport, Muxer, MuxerFactory};
use omuzaki_stream_api::packet::{Packet, SideKind, StreamId};

use super::webm::{WebmSink, WebmTrack};
use crate::vp9::alpha::ALPHA_BLOCK_ID;

/// A `WebM` file being written by stream copy.
pub struct WebmMuxer {
    path: PathBuf,
    sink: Option<WebmSink>,
    packets: u64,
    frames: u64,
}

impl Muxer for WebmMuxer {
    fn add_stream(&mut self, spec: &StreamSpec) -> Result<StreamId, StreamError> {
        let video = match (spec.kind, spec.video) {
            (StreamKind::Video, Some(video)) if self.sink.is_none() => video,
            (StreamKind::Video, _) => return Err(StreamError::Mux("a WebM clip holds one video stream".into())),
            _ => return Err(StreamError::Mux("a WebM clip's audio goes in WAV files beside it".into())),
        };
        let alpha = video.alpha != omuzaki_stream_api::encoder::AlphaKind::None;
        let track = WebmTrack { width: video.size.x, height: video.size.y, fps: video.fps, alpha };
        self.sink = Some(WebmSink::create(&self.path, track)?);
        Ok(StreamId(0))
    }

    fn write(&mut self, packet: &Packet) -> Result<(), StreamError> {
        let sink = self.sink.as_mut().ok_or_else(|| StreamError::Mux("no stream was added".into()))?;
        let index =
            u64::try_from(packet.pts).map_err(|_| StreamError::Mux("a packet before the clip's start".into()))?;
        let alpha = packet
            .side_data
            .iter()
            .find(|s| s.kind == SideKind::BlockAdditional { id: ALPHA_BLOCK_ID })
            .map(|s| s.data.as_ref());
        sink.write_frame(index, packet.keyframe, &packet.data, alpha)?;
        self.packets += 1;
        self.frames = self.frames.max(index + 1);
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<MuxReport, StreamError> {
        let sink = self.sink.take().ok_or_else(|| StreamError::Mux("no stream was added".into()))?;
        let bytes = sink.finish(self.frames)?;
        Ok(MuxReport::new(self.packets, bytes))
    }
}

/// Makes [`WebmMuxer`]s; refuses every container but `WebM`.
#[derive(Clone, Copy, Debug, Default)]
pub struct WebmMuxerFactory;

impl MuxerFactory for WebmMuxerFactory {
    fn create(&self, path: &Path, container: Container) -> Result<Box<dyn Muxer>, StreamError> {
        if container != Container::Webm {
            return Err(StreamError::Config(format!("the WebM writer can't write {container:?}")));
        }
        Ok(Box::new(WebmMuxer { path: path.to_owned(), sink: None, packets: 0, frames: 0 }))
    }
}
