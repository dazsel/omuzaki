//! [`FrameSlots`]: frames with ownership hand-back (`docs/design/supervisor.md` §4.3).
//!
//! A slot belongs to the producer until it announces it (`Frame { slot, index }`) and to the consumer until it hands
//! it back (`SlotFree { slot }`); those messages travel on the control channel, and this type only moves the bytes. A
//! slot is written in full before it is announced, so a producer killed mid-write never announces it, and a slot never
//! announced is never read.
//!
//! Layout: the tag, the slot count, the slot size, then one length word per slot, then the slots, each starting on a
//! cache line.

use std::sync::atomic::Ordering;

use crate::header::{self, WORD};
use crate::region::Region;
use crate::{Share, ShmError};

const KIND: u8 = 1;
/// Header words before the lengths: the tag, the slot count and the slot size.
const FIXED_WORDS: usize = 3;
/// The most slots a set may have (a recording's encode queue is 16).
const MAX_SLOTS: u32 = 256;

/// A set of frame slots for one worker generation.
#[derive(Debug)]
pub struct FrameSlots {
    region: Region,
    slots: u32,
    slot_bytes: usize,
    /// Each slot's stride: its size rounded up to a cache line.
    stride: usize,
    data: usize,
}

impl FrameSlots {
    /// `slots` slots of `slot_bytes` each (the host's end) and the [`Share`] that hands them to the other end.
    ///
    /// # Errors
    /// [`ShmError`] for no slots, more than 256, a zero slot size, or when the OS refuses.
    pub fn create(slots: u32, slot_bytes: usize) -> Result<(Self, Share), ShmError> {
        if slots == 0 || slots > MAX_SLOTS || slot_bytes == 0 {
            return Err(ShmError::new("create", format!("{slots} slots of {slot_bytes} bytes")));
        }
        let (stride, data, len) = Self::layout(slots, slot_bytes)?;
        let (region, share) = Region::create(len)?;
        header::write(&region, KIND, &[u64::from(slots), slot_bytes as u64])?;
        Ok((Self { region, slots, slot_bytes, stride, data }, share))
    }

    /// The other end of the slots `share` hands over.
    ///
    /// # Errors
    /// [`ShmError`] when the object is not a set of frame slots, or its header does not fit its size.
    pub fn open(share: Share) -> Result<Self, ShmError> {
        let region = Region::open(share)?;
        let geometry = header::read(&region, KIND, 2)?;
        let slots = u32::try_from(geometry[0]).map_err(|_| ShmError::new("open", "slot count"))?;
        let slot_bytes = header::size(geometry[1], "the slot size")?;
        if slots == 0 || slots > MAX_SLOTS || slot_bytes == 0 {
            return Err(ShmError::new("open", "the slot header is not valid"));
        }
        let (stride, data, len) = Self::layout(slots, slot_bytes)?;
        if len > region.len() {
            return Err(ShmError::new("open", "the slots are bigger than the object"));
        }
        Ok(Self { region, slots, slot_bytes, stride, data })
    }

    /// Each slot's stride, where the slots start, and the object's length.
    fn layout(slots: u32, slot_bytes: usize) -> Result<(usize, usize, usize), ShmError> {
        let too_big = || ShmError::new("create", "the slots do not fit this machine's address space");
        let stride = header::align(slot_bytes).ok_or_else(too_big)?;
        let data = header::align((FIXED_WORDS + slots as usize) * WORD).ok_or_else(too_big)?;
        let len = stride.checked_mul(slots as usize).and_then(|n| n.checked_add(data)).ok_or_else(too_big)?;
        Ok((stride, data, len))
    }

    /// How many slots there are.
    pub fn slots(&self) -> u32 {
        self.slots
    }

    /// The most bytes a slot holds.
    pub fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }

    fn slot(&self, slot: u32) -> Result<usize, ShmError> {
        if slot >= self.slots {
            return Err(ShmError::new("slot", format!("slot {slot} of {}", self.slots)));
        }
        Ok(self.data + slot as usize * self.stride)
    }

    fn len_word(&self, slot: u32) -> Result<&std::sync::atomic::AtomicU64, ShmError> {
        header::word(&self.region, FIXED_WORDS + slot as usize)
    }

    /// The producer writes `bytes` into `slot`, which it owns (the protocol's rule): the bytes first, then the length.
    ///
    /// # Errors
    /// [`ShmError`] for a slot out of range or more bytes than a slot holds.
    pub fn write(&self, slot: u32, bytes: &[u8]) -> Result<(), ShmError> {
        let at = self.slot(slot)?;
        if bytes.len() > self.slot_bytes {
            return Err(ShmError::new("write", format!("{} bytes into a {} byte slot", bytes.len(), self.slot_bytes)));
        }
        self.region.copy_in(at, bytes)?;
        self.len_word(slot)?.store(bytes.len() as u64, Ordering::Release);
        Ok(())
    }

    /// The consumer copies `slot`, which was announced to it, into `out`; returns how many bytes it held.
    ///
    /// # Errors
    /// [`ShmError`] for a slot out of range, a length the slot cannot hold (the producer wrote garbage), or an `out`
    /// shorter than the slot's bytes.
    pub fn read_into(&self, slot: u32, out: &mut [u8]) -> Result<usize, ShmError> {
        let at = self.slot(slot)?;
        let len = header::size(self.len_word(slot)?.load(Ordering::Acquire), "a slot length")?;
        if len > self.slot_bytes {
            return Err(ShmError::new("read", format!("slot {slot} claims {len} bytes of {}", self.slot_bytes)));
        }
        let out =
            out.get_mut(..len).ok_or_else(|| ShmError::new("read", format!("{len} bytes into a short buffer")))?;
        self.region.copy_out(at, out)?;
        Ok(len)
    }

    /// The bytes of `slot` as the consumer would read them, into a new buffer.
    ///
    /// # Errors
    /// As [`read_into`](Self::read_into).
    pub fn read(&self, slot: u32) -> Result<Vec<u8>, ShmError> {
        let mut out = vec![0; self.slot_bytes];
        let len = self.read_into(slot, &mut out)?;
        out.truncate(len);
        Ok(out)
    }

    /// Starts writing `bytes` into `slot` and stops halfway, as a producer killed mid-write would: half the bytes, no
    /// length (the crash tests).
    ///
    /// # Errors
    /// As [`write`](Self::write).
    #[cfg(feature = "testing")]
    pub fn write_torn(&self, slot: u32, bytes: &[u8]) -> Result<(), ShmError> {
        let at = self.slot(slot)?;
        let half = &bytes[..bytes.len().min(self.slot_bytes) / 2];
        self.region.copy_in(at, half)
    }

    /// Every byte of the object (the privacy test scans it).
    #[cfg(feature = "testing")]
    pub fn snapshot(&self) -> Vec<u8> {
        self.region.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_holds_what_was_written() -> Result<(), ShmError> {
        let (host, share) = FrameSlots::create(3, 100)?;
        let worker = FrameSlots::open(share)?;
        host.write(1, &[7; 100])?;
        host.write(2, b"short")?;
        assert_eq!(worker.read(1)?, vec![7; 100]);
        assert_eq!(worker.read(2)?, b"short");
        assert_eq!(worker.read(0)?, Vec::<u8>::new());
        assert!(host.write(3, b"x").is_err());
        assert!(host.write(0, &[0; 101]).is_err());
        let mut short = [0; 2];
        assert!(worker.read_into(2, &mut short).is_err());
        Ok(())
    }

    #[test]
    fn a_garbage_length_is_refused() -> Result<(), ShmError> {
        let (host, share) = FrameSlots::create(2, 16)?;
        let worker = FrameSlots::open(share)?;
        worker.len_word(0)?.store(u64::MAX, Ordering::Release);
        assert!(host.read(0).is_err());
        assert_eq!((worker.slots(), worker.slot_bytes()), (2, 16));
        Ok(())
    }

    #[test]
    fn bad_geometry_is_refused() {
        assert!(FrameSlots::create(0, 10).is_err());
        assert!(FrameSlots::create(2, 0).is_err());
        assert!(FrameSlots::create(MAX_SLOTS + 1, 10).is_err());
    }
}
