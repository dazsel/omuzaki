//! [`Latest`]: the newest-wins record (`docs/design/supervisor.md` §4.3): tracker frames, voice cues and live inputs.
//!
//! A seqlock over one record. The producer makes the sequence odd, writes, then makes it even; a reader copies the
//! record between two reads of the sequence and keeps it only when both are the same even value. **Reads are
//! bounded:** at most [`LATEST_READ_TRIES`] tries, then `None`, never a spin. A producer killed mid-write leaves the
//! sequence odd for good, so every later read of that generation returns `None` at once, which a consumer treats as a
//! silent source until it is attached to the next generation's object.
//!
//! Layout: the tag, the record's maximum size, the sequence, the record's length, then the record.

use std::sync::atomic::{Ordering, fence};

use crate::header;
use crate::region::Region;
use crate::{Share, ShmError};

const KIND: u8 = 2;
const SEQ: usize = 2;
const LEN: usize = 3;
const DATA: usize = header::DATA_ALIGN;

/// The most tries a read makes before it returns `None`.
pub const LATEST_READ_TRIES: u32 = 4;

/// One newest-wins record.
#[derive(Debug)]
pub struct Latest {
    region: Region,
    max_bytes: usize,
}

impl Latest {
    /// A record of at most `max_bytes` (the producer's or the host's end) and the [`Share`] for the other end.
    ///
    /// # Errors
    /// [`ShmError`] for a zero size, or when the OS refuses.
    pub fn create(max_bytes: usize) -> Result<(Self, Share), ShmError> {
        if max_bytes == 0 {
            return Err(ShmError::new("create", "a record needs at least one byte"));
        }
        let len = DATA.checked_add(max_bytes).ok_or_else(|| ShmError::new("create", "the record is too big"))?;
        let (region, share) = Region::create(len)?;
        header::write(&region, KIND, &[max_bytes as u64])?;
        Ok((Self { region, max_bytes }, share))
    }

    /// The other end of the record `share` hands over.
    ///
    /// # Errors
    /// [`ShmError`] when the object is not a record, or its header does not fit its size.
    pub fn open(share: Share) -> Result<Self, ShmError> {
        let region = Region::open(share)?;
        let max_bytes = header::size(header::read(&region, KIND, 1)?[0], "the record size")?;
        if max_bytes == 0 || DATA.checked_add(max_bytes).is_none_or(|len| len > region.len()) {
            return Err(ShmError::new("open", "the record is bigger than the object"));
        }
        Ok(Self { region, max_bytes })
    }

    /// The most bytes a record holds.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn word(&self, i: usize) -> Result<&std::sync::atomic::AtomicU64, ShmError> {
        header::word(&self.region, i)
    }

    /// The producer publishes `bytes` as the newest record; returns the new sequence number.
    ///
    /// # Errors
    /// [`ShmError`] for more bytes than the record holds.
    pub fn publish(&self, bytes: &[u8]) -> Result<u64, ShmError> {
        if bytes.len() > self.max_bytes {
            return Err(ShmError::new(
                "publish",
                format!("{} bytes into a {} byte record", bytes.len(), self.max_bytes),
            ));
        }
        let seq = self.word(SEQ)?;
        // One producer per generation, so the count is even here unless this producer itself was interrupted.
        let start = seq.load(Ordering::Relaxed) | 1;
        seq.store(start, Ordering::Relaxed);
        fence(Ordering::Release);
        self.region.store(DATA, bytes)?;
        self.word(LEN)?.store(bytes.len() as u64, Ordering::Relaxed);
        let end = start.wrapping_add(1);
        seq.store(end, Ordering::Release);
        Ok(end)
    }

    /// The consumer copies the newest whole record into `out`: `(seq, len)`, or `None` when nothing was published
    /// yet, or when every one of [`LATEST_READ_TRIES`] tries met a write in progress or a torn record. Never spins.
    /// `out` must hold [`max_bytes`](Self::max_bytes).
    pub fn read(&self, out: &mut [u8]) -> Option<(u64, usize)> {
        let (seq, len_word) = (self.word(SEQ).ok()?, self.word(LEN).ok()?);
        for _ in 0..LATEST_READ_TRIES {
            let before = seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                continue;
            }
            let Ok(len) = usize::try_from(len_word.load(Ordering::Relaxed)) else { continue };
            if len > self.max_bytes || len > out.len() {
                continue;
            }
            if self.region.load(DATA, &mut out[..len]).is_err() {
                continue;
            }
            fence(Ordering::Acquire);
            if seq.load(Ordering::Relaxed) == before {
                return Some((before, len));
            }
        }
        None
    }

    /// Starts publishing `bytes` and stops halfway, as a producer killed mid-write would (the crash tests).
    ///
    /// # Errors
    /// As [`publish`](Self::publish).
    #[cfg(feature = "testing")]
    pub fn publish_torn(&self, bytes: &[u8]) -> Result<(), ShmError> {
        let seq = self.word(SEQ)?;
        seq.store(seq.load(Ordering::Relaxed) | 1, Ordering::Release);
        let half = &bytes[..bytes.len().min(self.max_bytes) / 2];
        self.region.store(DATA, half)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_record_wins() -> Result<(), ShmError> {
        let (producer, share) = Latest::create(32)?;
        let consumer = Latest::open(share)?;
        let mut out = [0; 32];
        assert_eq!(consumer.read(&mut out), None);
        let first = producer.publish(b"one")?;
        let second = producer.publish(b"two, longer")?;
        assert!(second > first && second % 2 == 0);
        assert_eq!(consumer.read(&mut out), Some((second, 11)));
        assert_eq!(&out[..11], b"two, longer");
        assert!(producer.publish(&[0; 33]).is_err());
        Ok(())
    }

    #[test]
    fn a_garbage_length_reads_as_nothing() -> Result<(), ShmError> {
        let (producer, share) = Latest::create(8)?;
        let consumer = Latest::open(share)?;
        producer.publish(b"ok")?;
        producer.word(LEN)?.store(9_999, Ordering::Relaxed);
        let mut out = [0; 8];
        assert_eq!(consumer.read(&mut out), None);
        Ok(())
    }
}
