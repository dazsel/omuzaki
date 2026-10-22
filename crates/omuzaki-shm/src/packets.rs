//! [`PacketChannel`]: encoded packets from an encoder worker to the host (`docs/design/supervisor.md` §4.3).
//!
//! A single-producer, single-consumer byte ring of records. Each record is a fixed header (its length, the stream,
//! the keyframe flag, the side-data count, `pts`, `dts`, `duration`, the data length), then per side datum its kind,
//! id and length, then the data, then each side datum's bytes, padded to a word. The producer writes a record into
//! free space and only then advances `head`, so a producer killed mid-write leaves a record the consumer never sees:
//! only complete records are read. A full channel makes the producer wait (`send` returns `false`).
//!
//! Each end keeps its own counter (`head` for the producer, `tail` for the consumer) and publishes it for the other;
//! the consumer checks every count and length the producer wrote, so a broken or hostile worker produces an error,
//! never a fault in the host.
//!
//! Layout: the tag, the capacity, `head`, `tail`, then the ring.

use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};

use crate::header;
use crate::region::Region;
use crate::{Share, ShmError};

const KIND: u8 = 4;
const HEAD: usize = 2;
const TAIL: usize = 3;
const DATA: usize = header::DATA_ALIGN;
/// A record's fixed header.
const RECORD: usize = 40;
/// One side datum's header.
const SIDE: usize = 24;
/// The `BlockAdditional` side-data kind on the wire.
const BLOCK_ADDITIONAL: u8 = 1;

/// One packet channel.
#[derive(Debug)]
pub struct PacketChannel {
    region: Region,
    capacity: u64,
    /// This end's own counter: `head` on the producer, `tail` on the consumer.
    mine: AtomicU64,
}

fn pad(n: usize) -> Option<usize> {
    n.checked_add(7).map(|n| n / 8 * 8)
}

fn corrupt(detail: impl Into<String>) -> ShmError {
    ShmError::new("packet channel", detail)
}

impl PacketChannel {
    /// A channel of `capacity` bytes (the host's end, which consumes) and the [`Share`] for the producer.
    ///
    /// # Errors
    /// [`ShmError`] for a capacity under one record, or when the OS refuses.
    pub fn create(capacity: usize) -> Result<(Self, Share), ShmError> {
        let capacity = pad(capacity).ok_or_else(|| ShmError::new("create", "the channel is too big"))?;
        if capacity < RECORD {
            return Err(ShmError::new("create", format!("a {capacity} byte channel holds no record")));
        }
        let len = DATA.checked_add(capacity).ok_or_else(|| ShmError::new("create", "the channel is too big"))?;
        let (region, share) = Region::create(len)?;
        header::write(&region, KIND, &[capacity as u64])?;
        Ok((Self { region, capacity: capacity as u64, mine: AtomicU64::new(0) }, share))
    }

    /// The producer's end of the channel `share` hands over.
    ///
    /// # Errors
    /// [`ShmError`] when the object is not a packet channel, or its header does not fit its size.
    pub fn open(share: Share) -> Result<Self, ShmError> {
        let region = Region::open(share)?;
        let capacity = header::read(&region, KIND, 1)?[0];
        let size = header::size(capacity, "the capacity")?;
        if size < RECORD || size % 8 != 0 || DATA.checked_add(size).is_none_or(|len| len > region.len()) {
            return Err(ShmError::new("open", "the channel is bigger than the object"));
        }
        let head = header::word(&region, HEAD)?.load(Ordering::Acquire);
        Ok(Self { region, capacity, mine: AtomicU64::new(head) })
    }

    /// The channel's capacity in bytes.
    pub fn capacity(&self) -> usize {
        usize::try_from(self.capacity).unwrap_or(usize::MAX)
    }

    fn word(&self, i: usize) -> Result<&AtomicU64, ShmError> {
        header::word(&self.region, i)
    }

    /// Copies `bytes` into the ring at stream position `at`, wrapping.
    fn put(&self, at: u64, bytes: &[u8]) -> Result<(), ShmError> {
        let start = usize::try_from(at % self.capacity).map_err(|_| corrupt("position"))?;
        let first = bytes.len().min(self.capacity() - start);
        self.region.copy_in(DATA + start, &bytes[..first])?;
        self.region.copy_in(DATA, &bytes[first..])
    }

    /// Copies `out.len()` bytes out of the ring from stream position `at`, wrapping.
    fn get(&self, at: u64, out: &mut [u8]) -> Result<(), ShmError> {
        let start = usize::try_from(at % self.capacity).map_err(|_| corrupt("position"))?;
        let first = out.len().min(self.capacity() - start);
        let (a, b) = out.split_at_mut(first);
        self.region.copy_out(DATA + start, a)?;
        self.region.copy_out(DATA, b)
    }

    /// The record of `packet`.
    fn encode(packet: &Packet) -> Result<Vec<u8>, ShmError> {
        let sides = u16::try_from(packet.side_data.len()).map_err(|_| corrupt("too much side data"))?;
        let mut len = RECORD + SIDE * packet.side_data.len() + packet.data.len();
        len += packet.side_data.iter().map(|s| s.data.len()).sum::<usize>();
        let total = pad(len).ok_or_else(|| corrupt("the packet is too big"))?;
        let total32 = u32::try_from(total).map_err(|_| corrupt("the packet is too big"))?;
        let data_len = u32::try_from(packet.data.len()).map_err(|_| corrupt("the packet is too big"))?;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&total32.to_le_bytes());
        out.push(packet.stream.0);
        out.push(u8::from(packet.keyframe));
        out.extend_from_slice(&sides.to_le_bytes());
        out.extend_from_slice(&packet.pts.to_le_bytes());
        out.extend_from_slice(&packet.dts.to_le_bytes());
        out.extend_from_slice(&packet.duration.to_le_bytes());
        out.extend_from_slice(&data_len.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        for side in &packet.side_data {
            let SideKind::BlockAdditional { id } = side.kind else {
                return Err(corrupt(format!("side data {:?} has no wire form", side.kind)));
            };
            let side_len = u32::try_from(side.data.len()).map_err(|_| corrupt("the side data is too big"))?;
            out.push(BLOCK_ADDITIONAL);
            out.extend_from_slice(&[0; 7]);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&side_len.to_le_bytes());
            out.extend_from_slice(&[0; 4]);
        }
        out.extend_from_slice(&packet.data);
        for side in &packet.side_data {
            out.extend_from_slice(&side.data);
        }
        out.resize(total, 0);
        Ok(out)
    }

    /// The packet in `record`, every length checked against the record's.
    fn decode(record: &[u8]) -> Result<Packet, ShmError> {
        let u32_at = |i: usize| record.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let u64_at = |i: usize| {
            record.get(i..i + 8).map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        };
        let short = || corrupt("a record is shorter than its header says");
        let stream = StreamId(*record.get(4).ok_or_else(short)?);
        let keyframe = match record.get(5) {
            Some(0) => false,
            Some(1) => true,
            _ => return Err(corrupt("a keyframe flag is neither 0 nor 1")),
        };
        let sides =
            usize::from(u16::from_le_bytes([*record.get(6).ok_or_else(short)?, *record.get(7).ok_or_else(short)?]));
        let signed = |v: u64| i64::from_le_bytes(v.to_le_bytes());
        let pts = signed(u64_at(8).ok_or_else(short)?);
        let dts = signed(u64_at(16).ok_or_else(short)?);
        let duration = signed(u64_at(24).ok_or_else(short)?);
        let data_len = u32_at(32).ok_or_else(short)? as usize;
        let mut at = RECORD;
        let mut side_heads = Vec::with_capacity(sides.min(64));
        for _ in 0..sides {
            if *record.get(at).ok_or_else(short)? != BLOCK_ADDITIONAL {
                return Err(corrupt("an unknown side-data kind"));
            }
            let id = u64_at(at + 8).ok_or_else(short)?;
            let len = u32_at(at + 16).ok_or_else(short)? as usize;
            side_heads.push((id, len));
            at += SIDE;
        }
        let take = |at: &mut usize, len: usize| -> Result<Bytes, ShmError> {
            let end = at.checked_add(len).ok_or_else(short)?;
            let bytes = Bytes::copy_from_slice(record.get(*at..end).ok_or_else(short)?);
            *at = end;
            Ok(bytes)
        };
        let data = take(&mut at, data_len)?;
        let mut side_data = Vec::with_capacity(side_heads.len());
        for (id, len) in side_heads {
            side_data.push(SideData { kind: SideKind::BlockAdditional { id }, data: take(&mut at, len)? });
        }
        if pad(at) != Some(record.len()) {
            return Err(corrupt("a record's length does not match its contents"));
        }
        Ok(Packet { stream, pts, dts, duration, keyframe, data, side_data })
    }

    /// The producer sends `packet`: `Ok(false)` when the channel has no room for it yet (the producer waits and tries
    /// again).
    ///
    /// # Errors
    /// [`ShmError`] for a packet bigger than the channel, side data with no wire form, or a consumer count that
    /// cannot be.
    pub fn send(&self, packet: &Packet) -> Result<bool, ShmError> {
        let record = Self::encode(packet)?;
        if record.len() as u64 > self.capacity {
            return Err(corrupt(format!("a {} byte packet in a {} byte channel", record.len(), self.capacity)));
        }
        let head = self.mine.load(Ordering::Relaxed);
        let tail = self.word(TAIL)?.load(Ordering::Acquire);
        let used = head.checked_sub(tail).filter(|used| *used <= self.capacity).ok_or_else(|| corrupt("tail"))?;
        if self.capacity - used < record.len() as u64 {
            return Ok(false);
        }
        self.put(head, &record)?;
        let head = head + record.len() as u64;
        self.mine.store(head, Ordering::Relaxed);
        self.word(HEAD)?.store(head, Ordering::Release);
        Ok(true)
    }

    /// The consumer moves every complete record into `out` (appended); returns how many.
    ///
    /// # Errors
    /// [`ShmError`] when the producer's count or a record cannot be right (the worker is broken: kill it).
    pub fn recv(&self, out: &mut Vec<Packet>) -> Result<usize, ShmError> {
        let mut tail = self.mine.load(Ordering::Relaxed);
        let head = self.word(HEAD)?.load(Ordering::Acquire);
        if head < tail || head - tail > self.capacity {
            return Err(corrupt(format!("head {head} with tail {tail} in a {} byte channel", self.capacity)));
        }
        let mut n = 0;
        let mut len_bytes = [0_u8; 4];
        while tail < head {
            self.get(tail, &mut len_bytes)?;
            let len = u64::from(u32::from_le_bytes(len_bytes));
            if len < RECORD as u64 || len % 8 != 0 || len > head - tail {
                return Err(corrupt(format!("a {len} byte record with {} bytes written", head - tail)));
            }
            let mut record = vec![0; usize::try_from(len).map_err(|_| corrupt("record length"))?];
            self.get(tail, &mut record)?;
            out.push(Self::decode(&record)?);
            tail += len;
            n += 1;
        }
        self.mine.store(tail, Ordering::Relaxed);
        self.word(TAIL)?.store(tail, Ordering::Release);
        Ok(n)
    }

    /// The bytes written and not yet read, by the consumer's count.
    pub fn pending(&self) -> u64 {
        self.word(HEAD).map_or(0, |w| w.load(Ordering::Acquire)).saturating_sub(self.mine.load(Ordering::Relaxed))
    }

    /// Writes `packet`'s record but never advances `head`, as a producer killed mid-write would (the crash tests).
    ///
    /// # Errors
    /// As [`send`](Self::send).
    #[cfg(feature = "testing")]
    pub fn send_torn(&self, packet: &Packet) -> Result<(), ShmError> {
        let record = Self::encode(packet)?;
        self.put(self.mine.load(Ordering::Relaxed), &record[..record.len() / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pts: i64, data: &[u8], alpha: Option<&[u8]>) -> Packet {
        Packet {
            stream: StreamId(0),
            pts,
            dts: pts,
            duration: 1,
            keyframe: pts == 0,
            data: Bytes::copy_from_slice(data),
            side_data: alpha
                .map(|a| SideData { kind: SideKind::BlockAdditional { id: 1 }, data: Bytes::copy_from_slice(a) })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn packets_cross_with_their_side_data_and_the_ring_wraps() -> Result<(), ShmError> {
        let (host, share) = PacketChannel::create(256)?;
        let worker = PacketChannel::open(share)?;
        let mut got = Vec::new();
        let mut sent = Vec::new();
        for i in 0..40 {
            let p = packet(
                i,
                &vec![u8::try_from(i).unwrap_or(0); usize::try_from(i % 13).unwrap_or(0)],
                (i % 2 == 0).then_some(&b"alpha"[..]),
            );
            while !worker.send(&p)? {
                host.recv(&mut got)?;
            }
            sent.push(p);
        }
        host.recv(&mut got)?;
        assert_eq!(got, sent);
        assert_eq!(host.pending(), 0);
        Ok(())
    }

    #[test]
    fn a_full_channel_says_so_and_a_huge_packet_is_refused() -> Result<(), ShmError> {
        let (host, share) = PacketChannel::create(128)?;
        let worker = PacketChannel::open(share)?;
        assert!(worker.send(&packet(0, &[1; 40], None))?);
        assert!(!worker.send(&packet(1, &[1; 40], None))?);
        assert!(worker.send(&packet(2, &[1; 200], None)).is_err());
        let mut got = Vec::new();
        assert_eq!(host.recv(&mut got)?, 1);
        assert!(worker.send(&packet(1, &[1; 40], None))?);
        Ok(())
    }

    #[test]
    fn a_broken_producer_is_an_error_not_a_fault() -> Result<(), ShmError> {
        let (host, share) = PacketChannel::create(256)?;
        let worker = PacketChannel::open(share)?;
        worker.word(HEAD)?.store(10_000, Ordering::Release);
        assert!(host.recv(&mut Vec::new()).is_err());
        worker.word(HEAD)?.store(48, Ordering::Release);
        worker.put(0, &7_u32.to_le_bytes())?;
        assert!(host.recv(&mut Vec::new()).is_err());
        Ok(())
    }
}
