//! The memory muxer: keeps every packet and writes a small test container.
//!
//! The test container is the magic `OMZTEST1`, then one record per packet: a little-endian `u32` length, then the
//! stream (one byte), `pts`, `dts` and `duration` (little-endian `i64`), the keyframe flag (one byte) and the data. It
//! carries neither stream specs nor side data, so a muxer refuses packets with side data rather than drop them.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use bytes::Bytes;
use omuzaki_stream_api::encoder::{StreamKind, StreamSpec};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::mux::{Container, MuxReport, Muxer, MuxerFactory};
use omuzaki_stream_api::packet::{Packet, StreamId};

/// The first bytes of every test container.
const MAGIC: &[u8; 8] = b"OMZTEST1";
/// Bytes of a record before its data: stream, `pts`, `dts`, `duration` and the keyframe flag.
const RECORD_HEAD: usize = 1 + 8 + 8 + 8 + 1;

/// What the muxer checks per stream: whether it is video, and the last `dts` written.
struct StreamState {
    video: bool,
    last_dts: Option<i64>,
}

/// A muxer that keeps every packet in memory and checks them the way a real muxer must: a video stream starts on a
/// keyframe, and `dts` never goes back on a stream.
#[derive(Default)]
pub struct MemoryMuxer {
    specs: Vec<StreamSpec>,
    streams: Vec<StreamState>,
    packets: Vec<Packet>,
    started: bool,
}

impl MemoryMuxer {
    /// An empty muxer with no streams.
    pub fn new() -> Self {
        Self::default()
    }

    /// The packets written so far, in order.
    pub fn packets(&self) -> &[Packet] {
        &self.packets
    }

    /// The streams added so far, in id order.
    pub fn stream_specs(&self) -> &[StreamSpec] {
        &self.specs
    }
}

impl Muxer for MemoryMuxer {
    fn add_stream(&mut self, spec: &StreamSpec) -> Result<StreamId, StreamError> {
        if self.started {
            return Err(StreamError::Mux("streams are added before the first packet".into()));
        }
        spec.validate()?;
        let id = u8::try_from(self.streams.len())
            .map_err(|_| StreamError::Config("the test container holds at most 256 streams".into()))?;
        self.streams.push(StreamState { video: spec.kind == StreamKind::Video, last_dts: None });
        self.specs.push(spec.clone());
        Ok(StreamId(id))
    }

    fn write(&mut self, packet: &Packet) -> Result<(), StreamError> {
        let Some(state) = self.streams.get_mut(usize::from(packet.stream.0)) else {
            return Err(StreamError::Mux(format!("stream {} was not added", packet.stream.0)));
        };
        if !packet.side_data.is_empty() {
            return Err(StreamError::Mux("the test container carries no side data".into()));
        }
        if state.last_dts.is_none() && state.video && !packet.keyframe {
            return Err(StreamError::Mux("the first video packet must be a keyframe".into()));
        }
        if let Some(last) = state.last_dts
            && packet.dts < last
        {
            return Err(StreamError::Mux(format!("dts {} goes back to {last}", packet.dts)));
        }
        state.last_dts = Some(packet.dts);
        self.packets.push(packet.clone());
        self.started = true;
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<MuxReport, StreamError> {
        let bytes: usize = MAGIC.len() + self.packets.iter().map(|p| 4 + RECORD_HEAD + p.data.len()).sum::<usize>();
        let packets = u64::try_from(self.packets.len()).unwrap_or(u64::MAX);
        Ok(MuxReport::new(packets, u64::try_from(bytes).unwrap_or(u64::MAX)))
    }
}

/// Creates test-container files, which [`read_test_container`] reads back. The test format does not model container
/// differences, so every [`Container`] is written the same way.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryMuxerFactory;

impl MuxerFactory for MemoryMuxerFactory {
    fn create(&self, path: &Path, _container: Container) -> Result<Box<dyn Muxer>, StreamError> {
        let mut file = BufWriter::new(File::create(path).map_err(|e| io_error(&e))?);
        file.write_all(MAGIC).map_err(|e| io_error(&e))?;
        Ok(Box::new(FileMuxer { inner: MemoryMuxer::new(), file }))
    }
}

/// A [`MemoryMuxer`] that also writes each packet to its file as it arrives.
struct FileMuxer {
    inner: MemoryMuxer,
    file: BufWriter<File>,
}

impl Muxer for FileMuxer {
    fn add_stream(&mut self, spec: &StreamSpec) -> Result<StreamId, StreamError> {
        self.inner.add_stream(spec)
    }

    fn write(&mut self, packet: &Packet) -> Result<(), StreamError> {
        // Encoded first so a packet too large for a record is refused before the muxer keeps it.
        let record = encode_record(packet)?;
        self.inner.write(packet)?;
        self.file.write_all(&record).map_err(|e| io_error(&e))
    }

    fn finish(self: Box<Self>) -> Result<MuxReport, StreamError> {
        let Self { inner, mut file } = *self;
        file.flush().map_err(|e| io_error(&e))?;
        Box::new(inner).finish()
    }
}

/// Reads a test container back into its packets, in the order they were written.
///
/// # Errors
/// [`StreamError::Io`] if the file cannot be read; [`StreamError::Mux`] if it is not a test container or a record is
/// cut short.
pub fn read_test_container(path: &Path) -> Result<Vec<Packet>, StreamError> {
    let bytes = std::fs::read(path).map_err(|e| io_error(&e))?;
    let body = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(|| StreamError::Mux("not a test container".into()))?;
    let mut reader = Reader(body);
    let mut packets = Vec::new();
    while !reader.is_empty() {
        let len = usize::try_from(reader.u32()?).map_err(|_| truncated())?;
        packets.push(Reader(reader.take(len)?).packet()?);
    }
    Ok(packets)
}

/// The bytes of one record: a `u32` length, then the fields and the data.
fn encode_record(packet: &Packet) -> Result<Vec<u8>, StreamError> {
    let body_len = RECORD_HEAD + packet.data.len();
    let len = u32::try_from(body_len)
        .map_err(|_| StreamError::Mux(format!("a packet of {body_len} bytes does not fit a record")))?;
    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(packet.stream.0);
    out.extend_from_slice(&packet.pts.to_le_bytes());
    out.extend_from_slice(&packet.dts.to_le_bytes());
    out.extend_from_slice(&packet.duration.to_le_bytes());
    out.push(u8::from(packet.keyframe));
    out.extend_from_slice(&packet.data);
    Ok(out)
}

/// A cursor over container bytes; every read checks that enough bytes are left.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn take(&mut self, n: usize) -> Result<&[u8], StreamError> {
        if self.0.len() < n {
            return Err(truncated());
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], StreamError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, StreamError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, StreamError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Reads one record body; the rest of the body is the data.
    fn packet(&mut self) -> Result<Packet, StreamError> {
        let stream = StreamId(self.array::<1>()?[0]);
        let pts = self.i64()?;
        let dts = self.i64()?;
        let duration = self.i64()?;
        let keyframe = self.array::<1>()?[0] != 0;
        Ok(Packet { stream, pts, dts, duration, keyframe, data: Bytes::copy_from_slice(self.0), side_data: Vec::new() })
    }
}

fn truncated() -> StreamError {
    StreamError::Mux("the test container is cut short".into())
}

fn io_error(e: &std::io::Error) -> StreamError {
    StreamError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_core::glam::UVec2;
    use omuzaki_stream_api::encoder::{AlphaKind, CodecId, VideoSpec};
    use omuzaki_stream_api::time::Timebase;

    fn video_spec() -> StreamSpec {
        let video = VideoSpec { size: UVec2::new(8, 8), fps: (30, 1), alpha: AlphaKind::None };
        StreamSpec::video(CodecId::H264, "program", Timebase { num: 1, den: 30 }, video).unwrap()
    }

    /// Packets `0..n` on `stream`, a keyframe every 30 and a payload that differs per packet.
    fn sample_packets(stream: StreamId, n: i64) -> Vec<Packet> {
        (0..n)
            .map(|k| Packet {
                stream,
                pts: k,
                dts: k,
                duration: 1,
                keyframe: k % 30 == 0,
                data: Bytes::from(k.to_le_bytes().repeat(usize::try_from(k % 5 + 1).unwrap())),
                side_data: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn a_factory_file_reads_back_byte_for_byte_and_reports_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.omz");
        let mut muxer = MemoryMuxerFactory.create(&path, Container::Mkv).unwrap();
        let id = muxer.add_stream(&video_spec()).unwrap();
        let written = sample_packets(id, 100);
        for packet in &written {
            muxer.write(packet).unwrap();
        }
        let report = muxer.finish().unwrap();
        assert_eq!(report.packets, 100);
        assert_eq!(read_test_container(&path).unwrap(), written);
        assert_eq!(report.bytes, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn a_delta_frame_cannot_start_a_video_stream() {
        let mut muxer = MemoryMuxer::new();
        let id = muxer.add_stream(&video_spec()).unwrap();
        let delta = sample_packets(id, 2).remove(1);
        assert!(matches!(muxer.write(&delta), Err(StreamError::Mux(_))));
        assert_eq!(muxer.packets().len(), 0);
    }

    #[test]
    fn dts_going_backwards_is_refused_and_nothing_is_kept() {
        let mut muxer = MemoryMuxer::new();
        let id = muxer.add_stream(&video_spec()).unwrap();
        let packets = sample_packets(id, 3);
        muxer.write(&packets[0]).unwrap();
        muxer.write(&packets[2]).unwrap();
        assert!(matches!(muxer.write(&packets[1]), Err(StreamError::Mux(_))));
        assert_eq!(muxer.packets().len(), 2);
    }

    #[test]
    fn streams_must_be_added_before_packets_and_packets_must_name_a_stream() {
        let mut muxer = MemoryMuxer::new();
        let id = muxer.add_stream(&video_spec()).unwrap();
        assert!(matches!(muxer.write(&sample_packets(StreamId(1), 1)[0]), Err(StreamError::Mux(_))));
        muxer.write(&sample_packets(id, 1)[0]).unwrap();
        assert!(matches!(muxer.add_stream(&video_spec()), Err(StreamError::Mux(_))));
    }

    #[test]
    fn a_file_that_is_not_a_test_container_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.bin");
        std::fs::write(&path, b"not a container").unwrap();
        assert!(matches!(read_test_container(&path), Err(StreamError::Mux(_))));
    }
}
