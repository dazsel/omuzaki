//! The memory muxer: keeps every packet and writes a small test container.
//!
//! The test container is the magic `OMZTEST2`, then one record per packet, then an end marker. A record is a
//! little-endian `u32` length, then the stream (one byte), `pts`, `dts` and `duration` (little-endian `i64`), the
//! keyframe flag (one byte), the side data and the packet data. The side data is a `u32` count, then per chunk its kind
//! (one byte, `1` for `BlockAdditional`), the kind's `u64` id, a `u32` length and the bytes; the data is the rest of the
//! record. The end marker is the length `u32::MAX` and the `u64` packet count, so a file cut short, even at a record
//! boundary, reads back as an error. The container carries no stream specs.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use bytes::Bytes;
use omuzaki_stream_api::encoder::{StreamKind, StreamSpec};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::mux::{Container, MuxReport, Muxer, MuxerFactory};
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};

/// The first bytes of every test container.
const MAGIC: &[u8; 8] = b"OMZTEST2";
/// Bytes of a record before its side data: stream, `pts`, `dts`, `duration`, the keyframe flag and the side-data count.
const RECORD_HEAD: usize = 1 + 8 + 8 + 8 + 1 + 4;
/// Bytes of a side-data chunk before its bytes: kind, id and length.
const SIDE_HEAD: usize = 1 + 8 + 4;
/// The record length that marks the end of the container; no record is this long.
const END: u32 = u32::MAX;
/// Bytes of the end marker: [`END`] and the packet count.
const TRAILER: usize = 4 + 8;
/// The side-data kind byte for `BlockAdditional`.
const KIND_BLOCK_ADDITIONAL: u8 = 1;

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

/// The bytes of a record body (without its length): the head, the side data and the data.
fn body_len(packet: &Packet) -> usize {
    let side: usize = packet.side_data.iter().map(|s| SIDE_HEAD.saturating_add(s.data.len())).sum();
    RECORD_HEAD.saturating_add(side).saturating_add(packet.data.len())
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

    /// Checks `packet` the way [`Muxer::write`] does, without keeping it.
    fn check(&self, packet: &Packet) -> Result<(), StreamError> {
        let Some(state) = self.streams.get(usize::from(packet.stream.0)) else {
            return Err(StreamError::Mux(format!("stream {} was not added", packet.stream.0)));
        };
        if state.last_dts.is_none() && state.video && !packet.keyframe {
            return Err(StreamError::Mux("the first video packet must be a keyframe".into()));
        }
        if let Some(last) = state.last_dts
            && packet.dts < last
        {
            return Err(StreamError::Mux(format!("dts {} goes back to {last}", packet.dts)));
        }
        Ok(())
    }

    /// Keeps a packet that [`check`](Self::check) accepted.
    fn keep(&mut self, packet: &Packet) {
        if let Some(state) = self.streams.get_mut(usize::from(packet.stream.0)) {
            state.last_dts = Some(packet.dts);
        }
        self.packets.push(packet.clone());
        self.started = true;
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
        self.check(packet)?;
        self.keep(packet);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<MuxReport, StreamError> {
        let bytes: usize = MAGIC.len() + self.packets.iter().map(|p| 4 + body_len(p)).sum::<usize>() + TRAILER;
        let packets = u64::try_from(self.packets.len()).unwrap_or(u64::MAX);
        Ok(MuxReport::new(packets, u64::try_from(bytes).unwrap_or(u64::MAX)))
    }
}

/// Creates test-container files, which [`read_test_container`] reads back. The test format does not model container
/// differences, so every [`Container`] is written the same way.
#[derive(Clone, Copy, Debug, Default)]
pub struct MemoryMuxerFactory;

impl MuxerFactory for MemoryMuxerFactory {
    #[allow(clippy::disallowed_methods, reason = "the testing kit writes the file its test names; it is not an op")]
    fn create(&self, path: &Path, _container: Container) -> Result<Box<dyn Muxer>, StreamError> {
        let mut file = BufWriter::new(File::create(path).map_err(|e| io_error(&e))?);
        file.write_all(MAGIC).map_err(|e| io_error(&e))?;
        Ok(Box::new(FileMuxer { inner: MemoryMuxer::new(), file, failed: false }))
    }
}

/// A [`MemoryMuxer`] that also writes each packet to its file as it arrives.
///
/// The end marker is written by `finish` only. A muxer dropped without `finish` leaves a file with no end marker, which
/// [`read_test_container`] refuses, so a flush error lost in the drop cannot pass as a complete file.
struct FileMuxer {
    inner: MemoryMuxer,
    file: BufWriter<File>,
    /// A write failed, so the file may end in half a record; later writes and `finish` fail too.
    failed: bool,
}

impl Muxer for FileMuxer {
    fn add_stream(&mut self, spec: &StreamSpec) -> Result<StreamId, StreamError> {
        self.inner.add_stream(spec)
    }

    fn write(&mut self, packet: &Packet) -> Result<(), StreamError> {
        if self.failed {
            return Err(failed());
        }
        // Checked and encoded first: a refused packet touches neither the file nor the packets kept, and the packet is
        // kept only once its record is in the file.
        self.inner.check(packet)?;
        let record = encode_record(packet)?;
        if let Err(e) = self.file.write_all(&record) {
            self.failed = true;
            return Err(io_error(&e));
        }
        self.inner.keep(packet);
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<MuxReport, StreamError> {
        let Self { inner, mut file, failed: was_failed } = *self;
        if was_failed {
            return Err(failed());
        }
        let count = u64::try_from(inner.packets.len()).unwrap_or(u64::MAX);
        file.write_all(&END.to_le_bytes()).map_err(|e| io_error(&e))?;
        file.write_all(&count.to_le_bytes()).map_err(|e| io_error(&e))?;
        file.flush().map_err(|e| io_error(&e))?;
        Box::new(inner).finish()
    }
}

/// Reads a test container back into its packets, in the order they were written.
///
/// # Errors
/// [`StreamError::Io`] if the file cannot be read; [`StreamError::Mux`] if it is not a test container, a record is cut
/// short, the end marker is missing or its count disagrees, or bytes follow the end marker.
#[allow(clippy::disallowed_methods, reason = "the testing kit reads the file its test names; it is not an op")]
pub fn read_test_container(path: &Path) -> Result<Vec<Packet>, StreamError> {
    let bytes = std::fs::read(path).map_err(|e| io_error(&e))?;
    let body = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(|| StreamError::Mux("not a test container".into()))?;
    let mut reader = Reader(body);
    let mut packets = Vec::new();
    loop {
        let len = reader.u32()?;
        if len == END {
            break;
        }
        let len = usize::try_from(len).map_err(|_| truncated())?;
        packets.push(Reader(reader.take(len)?).packet()?);
    }
    let count = reader.u64()?;
    if !reader.is_empty() {
        return Err(StreamError::Mux("bytes follow the end of the test container".into()));
    }
    if u64::try_from(packets.len()) != Ok(count) {
        return Err(StreamError::Mux(format!(
            "the end marker counts {count} packets, the file holds {}",
            packets.len()
        )));
    }
    Ok(packets)
}

/// The bytes of one record: a `u32` length, then the fields, the side data and the data.
fn encode_record(packet: &Packet) -> Result<Vec<u8>, StreamError> {
    let body_len = body_len(packet);
    let too_big = || StreamError::Mux(format!("a packet of {body_len} bytes does not fit a record"));
    let len = u32::try_from(body_len).map_err(|_| too_big())?;
    if len == END {
        return Err(too_big());
    }
    let mut out = Vec::with_capacity(4 + body_len);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(packet.stream.0);
    out.extend_from_slice(&packet.pts.to_le_bytes());
    out.extend_from_slice(&packet.dts.to_le_bytes());
    out.extend_from_slice(&packet.duration.to_le_bytes());
    out.push(u8::from(packet.keyframe));
    let count = u32::try_from(packet.side_data.len()).map_err(|_| too_big())?;
    out.extend_from_slice(&count.to_le_bytes());
    for side in &packet.side_data {
        // `SideKind` is non-exhaustive: a kind this format cannot write is refused, never dropped.
        let SideKind::BlockAdditional { id } = side.kind else {
            return Err(StreamError::Mux("the test container cannot carry this side-data kind".into()));
        };
        out.push(KIND_BLOCK_ADDITIONAL);
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&u32::try_from(side.data.len()).map_err(|_| too_big())?.to_le_bytes());
        out.extend_from_slice(&side.data);
    }
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

    fn u64(&mut self) -> Result<u64, StreamError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, StreamError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// Reads one record body; the rest of the body after the side data is the data.
    fn packet(&mut self) -> Result<Packet, StreamError> {
        let stream = StreamId(self.array::<1>()?[0]);
        let pts = self.i64()?;
        let dts = self.i64()?;
        let duration = self.i64()?;
        let keyframe = self.array::<1>()?[0] != 0;
        let count = self.u32()?;
        // Not sized from `count`: a corrupt count must fail on the missing bytes, not allocate.
        let mut side_data = Vec::new();
        for _ in 0..count {
            let [kind] = self.array::<1>()?;
            if kind != KIND_BLOCK_ADDITIONAL {
                return Err(StreamError::Mux(format!("unknown side-data kind {kind}")));
            }
            let id = self.u64()?;
            let len = usize::try_from(self.u32()?).map_err(|_| truncated())?;
            side_data.push(SideData {
                kind: SideKind::BlockAdditional { id },
                data: Bytes::copy_from_slice(self.take(len)?),
            });
        }
        Ok(Packet { stream, pts, dts, duration, keyframe, data: Bytes::copy_from_slice(self.0), side_data })
    }
}

fn truncated() -> StreamError {
    StreamError::Mux("the test container is cut short".into())
}

fn failed() -> StreamError {
    StreamError::Mux("an earlier write to the test container failed".into())
}

fn io_error(e: &std::io::Error) -> StreamError {
    StreamError::Io(e.to_string())
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, reason = "tests set up and cut their fixture files on disk; the fence is for ops")]
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

    /// `sample_packets` where every third packet carries a `BlockAdditional` chunk and every seventh two of them.
    fn packets_with_side_data(stream: StreamId, n: i64) -> Vec<Packet> {
        let mut packets = sample_packets(stream, n);
        for (k, p) in (0..).zip(packets.iter_mut()) {
            let chunk = |id: u64, len: usize| SideData {
                kind: SideKind::BlockAdditional { id },
                data: Bytes::from(vec![u8::try_from(k % 251).unwrap(); len]),
            };
            if k % 3 == 0 {
                p.side_data.push(chunk(1, usize::try_from(k % 4).unwrap()));
            }
            if k % 7 == 0 {
                p.side_data.push(chunk(u64::MAX, 5));
            }
        }
        packets
    }

    /// A finished container holding `packets`, as bytes.
    fn container_bytes(dir: &Path, packets: &[Packet]) -> (std::path::PathBuf, Vec<u8>) {
        let path = dir.join("take.omz");
        let mut muxer = MemoryMuxerFactory.create(&path, Container::Mkv).unwrap();
        muxer.add_stream(&video_spec()).unwrap();
        for packet in packets {
            muxer.write(packet).unwrap();
        }
        muxer.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    }

    #[test]
    fn side_data_survives_the_container_and_the_memory_muxer_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let written = packets_with_side_data(StreamId(0), 30);
        assert!(written.iter().any(|p| p.side_data.len() == 2) && written.iter().any(|p| p.side_data.is_empty()));
        let (path, _) = container_bytes(dir.path(), &written);
        assert_eq!(read_test_container(&path).unwrap(), written);

        let mut memory = MemoryMuxer::new();
        memory.add_stream(&video_spec()).unwrap();
        for packet in &written {
            memory.write(packet).unwrap();
        }
        assert_eq!(memory.packets(), written);
        let report = Box::new(memory).finish().unwrap();
        assert_eq!(report.bytes, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn a_file_cut_short_anywhere_is_an_error_not_a_complete_take() {
        let dir = tempfile::tempdir().unwrap();
        let packets = packets_with_side_data(StreamId(0), 5);
        let (path, bytes) = container_bytes(dir.path(), &packets);
        // The first record ends here, so this cut falls on a record boundary.
        let boundary = MAGIC.len() + 4 + body_len(&packets[0]);
        for cut in [boundary, bytes.len() - TRAILER, bytes.len() - 1, MAGIC.len()] {
            std::fs::write(&path, &bytes[..cut]).unwrap();
            assert!(matches!(read_test_container(&path), Err(StreamError::Mux(_))), "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        std::fs::write(&path, &longer).unwrap();
        assert!(matches!(read_test_container(&path), Err(StreamError::Mux(_))));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read_test_container(&path).unwrap(), packets);
    }

    #[test]
    fn a_muxer_dropped_without_finish_leaves_a_file_the_reader_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dropped.omz");
        let mut muxer = MemoryMuxerFactory.create(&path, Container::Mkv).unwrap();
        let id = muxer.add_stream(&video_spec()).unwrap();
        muxer.write(&sample_packets(id, 1)[0]).unwrap();
        drop(muxer);
        assert!(matches!(read_test_container(&path), Err(StreamError::Mux(_))));
    }

    #[test]
    fn a_refused_packet_reaches_neither_the_file_nor_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refused.omz");
        let mut muxer = MemoryMuxerFactory.create(&path, Container::Mkv).unwrap();
        let id = muxer.add_stream(&video_spec()).unwrap();
        let packets = sample_packets(id, 3);
        assert!(muxer.write(&packets[1]).is_err());
        muxer.write(&packets[0]).unwrap();
        let report = muxer.finish().unwrap();
        assert_eq!(report.packets, 1);
        assert_eq!(read_test_container(&path).unwrap(), packets[..1]);
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
