//! A lock-free single-producer single-consumer ring of `f32` samples: the hand-off from the audio callback to the
//! VAD thread.
//!
//! Pushing never allocates, locks or blocks, so it is safe in a real-time callback. Samples are stored as the bits of
//! an `f32` in `AtomicU32` slots, which keeps the whole thing in safe Rust (the crate denies `unsafe`): the producer
//! writes slots with relaxed stores and publishes them with a release store of the write counter; the consumer
//! acquires the counter before reading the slots, and publishes what it consumed the same way in the other direction.
//!
//! **Back-pressure policy:** when the consumer falls behind and the ring is full, the *newest* samples that do not fit
//! are dropped (the audio already queued stays contiguous) and counted in [`Producer::dropped`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

struct Shared {
    slots: Box<[AtomicU32]>,
    /// Total samples ever written. Only the producer stores to it.
    written: AtomicU64,
    /// Total samples ever read. Only the consumer stores to it.
    read: AtomicU64,
    /// Total samples dropped because the ring was full.
    dropped: AtomicU64,
}

/// The write end. Move it into the audio callback.
pub struct Producer {
    shared: Arc<Shared>,
}

/// The read end. Owned by the VAD thread.
pub struct Consumer {
    shared: Arc<Shared>,
}

/// Creates a ring that holds `capacity` samples (at least one).
#[must_use]
pub fn ring(capacity: usize) -> (Producer, Consumer) {
    let slots = (0..capacity.max(1)).map(|_| AtomicU32::new(0)).collect();
    let shared =
        Arc::new(Shared { slots, written: AtomicU64::new(0), read: AtomicU64::new(0), dropped: AtomicU64::new(0) });
    (Producer { shared: Arc::clone(&shared) }, Consumer { shared })
}

impl Shared {
    fn capacity(&self) -> u64 {
        self.slots.len() as u64
    }

    fn slot(&self, index: u64) -> &AtomicU32 {
        // The modulo result is below `slots.len()`, so the index is in range and fits `usize`.
        &self.slots[(index % self.capacity()) as usize]
    }
}

impl Producer {
    /// Appends as many of `samples` as fit and returns how many were accepted; the rest are dropped and counted.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.shared;
        let written = shared.written.load(Ordering::Relaxed);
        let read = shared.read.load(Ordering::Acquire);
        let free = shared.capacity() - (written - read);
        let accepted = samples.len().min(usize::try_from(free).unwrap_or(usize::MAX));
        for (offset, sample) in samples[..accepted].iter().enumerate() {
            shared.slot(written + offset as u64).store(sample.to_bits(), Ordering::Relaxed);
        }
        shared.written.store(written + accepted as u64, Ordering::Release);
        if accepted < samples.len() {
            shared.dropped.fetch_add((samples.len() - accepted) as u64, Ordering::Relaxed);
        }
        accepted
    }

    /// Samples that fit right now. A feeder that must not lose audio (a file played faster than real time) waits until
    /// this covers its block before pushing.
    #[must_use]
    pub fn free(&self) -> usize {
        let shared = &*self.shared;
        let in_use = shared.written.load(Ordering::Relaxed) - shared.read.load(Ordering::Acquire);
        usize::try_from(shared.capacity() - in_use).unwrap_or(usize::MAX)
    }

    /// Total samples dropped so far because the ring was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

impl Consumer {
    /// Moves up to `out.len()` samples into `out` and returns how many were read.
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let shared = &*self.shared;
        let read = shared.read.load(Ordering::Relaxed);
        let written = shared.written.load(Ordering::Acquire);
        let n = out.len().min(usize::try_from(written - read).unwrap_or(usize::MAX));
        for (offset, slot) in out[..n].iter_mut().enumerate() {
            *slot = f32::from_bits(shared.slot(read + offset as u64).load(Ordering::Relaxed));
        }
        shared.read.store(read + n as u64, Ordering::Release);
        n
    }

    /// Total samples dropped so far because the ring was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_come_out_in_order_across_the_wrap() {
        let (mut tx, mut rx) = ring(8);
        let mut out = [0.0; 8];
        let mut next = 0.0f32;
        for _ in 0..10 {
            let block: Vec<f32> = (0..5)
                .map(|_| {
                    next += 1.0;
                    next
                })
                .collect();
            assert_eq!(tx.push(&block), 5);
            assert_eq!(rx.pop_into(&mut out[..5]), 5);
            assert_eq!(&out[..5], block.as_slice());
        }
    }

    #[test]
    fn a_full_ring_drops_the_newest_samples_and_counts_them() {
        let (mut tx, mut rx) = ring(4);
        assert_eq!(tx.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(tx.push(&[4.0, 5.0, 6.0]), 1);
        assert_eq!((tx.dropped(), rx.dropped()), (2, 2));
        let mut out = [0.0; 8];
        assert_eq!(rx.pop_into(&mut out), 4);
        assert_eq!(&out[..4], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(rx.pop_into(&mut out), 0);
    }

    #[test]
    fn two_threads_see_every_sample_once_and_in_order() {
        let (mut tx, mut rx) = ring(64);
        let total = 200_000u32;
        let producer = std::thread::spawn(move || {
            let mut sent = 0u32;
            while sent < total {
                let end = (sent + 37).min(total);
                let block: Vec<f32> = (sent..end).map(|v| v as f32).collect();
                sent += u32::try_from(tx.push(&block)).unwrap_or(0);
                // A partly accepted block resumes from the first dropped sample, so nothing is lost or repeated.
                std::thread::yield_now();
            }
        });
        let mut expected = 0u32;
        let mut buf = [0.0f32; 50];
        while expected < total {
            let n = rx.pop_into(&mut buf);
            for v in &buf[..n] {
                assert_eq!(*v, expected as f32);
                expected += 1;
            }
        }
        producer.join().unwrap();
    }
}
